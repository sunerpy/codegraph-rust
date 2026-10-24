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
- **Razor `@using` cascade resolution** — `resolveRazorUsing` / `getRazorUsings`
  (`index.ts:1116-1158`), tied to the Razor framework layer.

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

Upstream #1820 extends that same model to statically receiver-qualified method
values in TypeScript/JavaScript and Python. `obj.method` and proven
`this.field.method` callbacks reuse the ordinary conservative method target
selection, but the persisted edge remains `References` with `fnRef:true`; no
immediate `Calls` edge is fabricated. Call-result, index, literal, missing-type,
and ambiguous receiver shapes remain unresolved.

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

The Task 22 parity corpus compares the 8 upstream tools; the Rust server also
exposes the additive `codegraph_check` and `codegraph_export` tools. Those 8
shared tools render byte-identically on the mini golden EXCEPT the following
text-formatting / relevance-ordering differences. Six of the eight shared tools (`codegraph_search`, `codegraph_callers`,
`codegraph_callees`, `codegraph_impact`, `codegraph_node` symbol+file modes,
`codegraph_files` tree/flat/grouped) are byte-identical; the diffs below are
confined to `codegraph_explore` and `codegraph_status`.

- **`codegraph_explore` relevance ranking (RWR/skeletonization) is not
  ported; the size-adaptive output BUDGET and the Flow section are.** colby's
  `findRelevantContext` (`reference/colby/src/context/index.ts`) ranks the
  subgraph with Random-Walk-with-Restart / personalized PageRank, computes a
  FLOW spine among named symbols (`buildFlowFromNamedSymbols`), and adaptively
  SKELETONIZES off-spine polymorphic-sibling files to signatures
  (`adaptiveExplore`, `tools.ts:2422-2623`). The port opens the response with
  the same Flow section (the chain, `dynamic:` hop tags, `(when …)` branch
  conditions). The spine does not yet weight file ranking or body windows, the
  "Dynamic-dispatch links among your symbols" section is not printed, and a
  flow that does not connect gets no boundary or polymorphic-dispatch
  announcement of its own; the port's boundary scan over the explore seeds
  still runs. The Rust port reproduces the DETERMINISTIC structure — header,
  blast radius, relationship map, dynamic boundaries, and per-file source
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

## Kept different on purpose (KEEP-RUST)

Where the port reads a shape differently from colby on purpose, the decision is
listed here with the module that carries it; the behavior itself is described
in the language rows of [`languages.md`](../languages.md), the corpus sections
of [`equivalence.md`](../equivalence.md) and the module docs. Each one leaves
a guess unresolved, gives the same graph whatever order the files were indexed
in, reads the source as it is written where colby misreads it, follows a
grammar or extractor shape that is not colby's, or, by the owner's decision,
records what colby does not. None is a Tier-3 rule: the goldens pin this
port's own output, these differences included. A difference of mechanism
alone, which gives colby's result (one scan in place of a regex compiled per
name, a per-pass memo, a pattern rewritten for Rust's regex engine), is not
listed.

### Resolution

- **Receiver words (#226, OD-8).** A method call on a receiver nothing types
  binds a single or best candidate only when the receiver shares a word with
  the owner, where colby guesses. For the untyped-receiver languages (Ruby,
  CFML, Objective-C, PHP) colby's owner-word rule also admits one
  (`name_matcher.rs`).
- **Nested receivers (#1496, G14, OD-8).** A call down a longer chain
  (`a.b.c()`, and TS/JS `this.a.b.m()`) keeps its last receiver segment (`b.c`)
  where colby records the bare `c`. Scala, Swift and Go read colby's link back
  from the source and hold the result to colby's chain rule (`scala_scope.rs`,
  `swift_scope.rs`, `go_packages.rs`).
- **Rust calls that lost their receiver (P15, the Rust half of #2167, OD-8).** A
  call the extractor recorded under a bare name binds only a same-named method,
  never through a `use`, a fuzzy guess or a std method's name; a bare `Ok`,
  `Err`, `Some` or `None` is the prelude's unless a `use` in scope imports a
  project variant (`name_matcher.rs`, `rust_lexical.rs`).
- **Untyped Python method values (#2218, OD-8).** `self.store.fetch` stays
  unresolved even when one project method bears the name (`member_value.rs`).
- **C# extension methods (W3-05c, owner 2026-10-11, OD-8).** A call on an
  outside type reaches the one project extension method in scope that takes it
  as `this`; colby resolves no extension method (`csharp_extensions.rs`).
- **A test's NestJS bootstrap (W7-02b).** A `NestFactory.create` in a test
  file (an e2e spec) with no `src/` segment above it belongs to the directory
  above its test folder, else the one holding it, and within one app the
  entry point's bootstrap decides over a test's: an e2e spec's
  `setGlobalPrefix('api')` names neither another integration app's routes
  nor the routes its app's own entry point serves. colby puts every such
  spec at the repository root and prefers a bootstrap that configures the
  app, so all of nestjs/nest's integration apps took one spec's `/api`
  (`nestjs.rs`).
- **Dart named constructors (#2380).** A named constructor, indexed as a
  method, is reached only by a call written through its class (`Foo.value(2)`).
  A call through a value of that type (a typed parameter or local, a chain's
  last link), or through a receiver the matcher reads as the class (`foo`) or
  scores by its words (`fooValue`), calls an instance or extension member of
  that name, or nothing, where colby's member lookup and receiver guesses take
  the constructor (`dart_members.rs`, `name_matcher.rs`).

### Extraction

- **Value references (#897, OD-8).** Always on: colby's
  `CODEGRAPH_VALUE_REFS=0` switch is not ported (`value_refs.rs`).
- **Property types (W2-03d, owner 2026-10-11, OD-8).** A Kotlin or PHP
  property's declared type is a `references` edge from its field; colby reads
  none. A Swift property's type annotation is a reference too (W2-03c), which in
  `extension Socket.Option { static let none: Option = … }` reaches
  `Socket::Option`, an edge colby's graph lacks.
- **C/C++ alias signatures (W4-CPP-04, owner 2026-10-12, OD-8).** A
  `type_alias` node's `signature` is its declaration as written; colby records
  no aliased type (`walker.rs`).
- **C/C++ pre-parse.** In C++ the C leading attribute macro is blanked only
  with an object-like `#define` proving it; the inline-macro pass never blanks
  inside a string literal (colby spares raw strings only); lone macro lines are
  blanked in C++ only (`lang/cpp.rs`, `lang/cpp_preparse.rs`).
- **CFML fragments.** A `<cfscript>` or `<cfquery>` body is walked in the
  file's own tree, so its ids and columns are the file's (colby re-parses the
  text and shifts its lines); a function re-kinded as a method gets a method's
  id; a top-level `var` local is no node (`walker.rs`).
- **MyBatis.** A self-closing `<sqlMap resource="…"/>` is a config's list
  entry, never a map's root (`embedded/mybatis.rs`).
- **Dart positions.** The later links of a call chain, and a call through an
  import prefix, are recorded where the chain starts (colby records them past
  the name), and resolution reads them back from there; a function declared in
  a body is qualified by the function around it (`outer::make`, colby's is
  `make`); local-scope offsets are bytes, colby's UTF-16 code units
  (`dart_chains.rs`, `dart_prefixes.rs`, `dart_local_scope.rs`,
  `dart_scope.rs`).

### Path order, not index order

Where a walk is capped or the first declaration wins, colby reads same-named
declarations in index order and the port in path order, so the cap cuts in
the same place on every index: Java supertypes (#2126), Kotlin declarations,
typealiases and receiver files, Ruby and PHP ancestries, CFML component files,
Swift's `declarationFor` and Objective-C supertypes. C/C++ namespace aliases,
openers, closers and macro aliases take the first definition in path order
(colby takes the last macro alias and closer), and same-named aliases from two
`#if` branches the first in byte order of path (colby sorts with
`localeCompare`). Java's ancestor walk (#2236) reads each type once, at the
nearest level, where colby memoises a type at the depth it was first asked for
and so depends on resolution order; a Lua module member is memoised per lookup
depth for the same reason (`lua_scope.rs`).

### The source as written

- **Java anonymous classes.** `new LinkedTreeMapIterator<…>() { … }` has the
  type it instantiates as its supertype, so its body's calls reach the base's
  methods; colby reads only the declaration's head (`java_scope.rs`).
- **Solidity `is` clauses.** A `//` comment drops only the rest of its line;
  colby joins the lines first, so a comment in a head drops the bases below it
  (`solidity_scope.rs`).
- **Erlang `-import` lists.** Read entry by entry, a quoted atom whole, so
  `foo/2` inside `'x,foo'/2` is not imported (`erlang_imports.rs`).
- **C/C++ macro visibility.** An `#if` expression is evaluated whole (colby
  reads a literal, one `defined` test or a bare name), and only a whole-file
  `#ifndef X` / empty `#define X` pair is an include guard, so a feature-flag
  default or a fallback function-like macro hides no call
  (`c_macro_visibility.rs`).
- **JS/TS.** A store action binds only through a store a Zustand factory
  builds on its initializer's call path (colby checks that for selectors only,
  `js_store.rs`); a field ending in `$` is a whole token (`name_matcher.rs`);
  local and default export aliases take Unicode identifiers, colby's ASCII ones
  only (`import_resolver.rs`).
- **PHP.** A class name reference is recorded by its last segment and its
  written name read back from the source; a rooted `\strlen()` keeps its `\`
  and resolves as `strlen` (`php_scope.rs`, `import_resolver.rs`).
- **Dart libraries.** Only an indexed file's directives are read, a URI whose
  path leaves the project names nothing, an export chain is followed at most 64
  libraries deep (colby recurses without a bound), and a field's initializer is
  read only at its class's member level (`dart_libraries.rs`,
  `dart_chains.rs`).
- **Shopify themes.** A marker path is probed only once it is proven lexically
  inside the project, and marker names match in any case
  (`embedded/shopify.rs`).
- **Pascal `.inc`.** An `.inc` override turns the content sniff off and the
  file stays PHP (an override never claims a built-in extension), and a file
  over the size limit keeps its extension's language (`pascal_include.rs`).

### Sync lanes

Several rules above read text across files, so an edit can change what a
reference in an unchanged file resolves to. The watch crate keeps sync lanes
for them that colby's sync has no counterpart for, each with a version marker
withdrawn before the sync's first write and restored once it has resolved
([Daemon and watch lifecycle](../architecture.md#daemon-and-watch-lifecycle)).
The Shopify theme lane also covers a scoped sync: a theme marker added or
removed converges there, where colby's needs a full sync. NestJS route naming
needs no lane: every sync renames the routes again, and one that nothing
renames any more (its app's global prefix or version, or its module's
`RouterModule` entry, went away) gets the name its file gives it back
(`frameworks/nestjs.rs`), where colby's sync keeps the old name.
