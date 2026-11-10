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
(`index.ts`), plus concrete `FrameworkResolver` implementations behind the
`FrameworkResolver` trait EXTENSION POINT (`crates/codegraph-resolve/src/framework.rs`):
Laravel, Drupal, Express, NestJS, React, Vue, Godot, Tauri, Nuxt, React Router, TanStack Router,
Next.js, Svelte, SvelteKit, Vue Router, Angular Router, Astro, Django, Flask,
FastAPI, Rails, Spring, Play, Expo Router and GoFrame (`crates/codegraph-resolve/src/frameworks/`), detected per-project by
`detect_frameworks`. The other colby resolvers stay deferred. The following
colby behaviors are intentionally NOT ported in v1:

- **The other framework-specific resolvers** — every concrete `FrameworkResolver`
  in `reference/colby/src/resolution/frameworks/**` EXCEPT those above:
  React Native / Expo / Fabric bridges,
  Swift↔ObjC bridging, and the Go-module / C# route
  extractors. These are an additive heuristic layer; on a project where none
  of them is detected the orchestrator holds an empty resolver list, so
  behavior matches colby with zero frameworks detected. Astro's extraction
  (frontmatter + `<script>` delegation, template `{call(...)}` and
  `<Component>` refs) is the embedded extractor's
  (`crates/codegraph-extract/src/embedded/astro.rs`, #768), and its framework
  resolution (`astro:*` module imports, the `Astro` global, `src/pages` routes,
  components named in markup) the Astro resolver's (`frameworks/astro.rs`).
  Re-enable any of the deferred resolvers by adding it to `frameworks/` and the
  `detect_frameworks` registry — no orchestrator change required.
- **Django's ORM dispatch (W7-19b)** — colby's Django resolver links
  `_iterable_class` to `ModelIterable.__iter__` (0.7), written for Django's own
  source. Its answer reads the class named `ModelIterable` while its edge's
  target is `__iter__`, so a second `ModelIterable`, or that class gaining an
  `__iter__`, changes the answer where a sync finds the edge by neither the
  target's name nor the reference's. It waits for a sync lane that reads it;
  Django claims no `_iterable_class` (`frameworks/python.rs`).
- **FastAPI mounts through an absolute import (W7-19b)** — a router mounted
  through an absolute import (`from app.api.routes import items`, then
  `api_router.include_router(items.router)`) keeps its routes' file names:
  the port finds the mounted module as colby's `b635dd46` does, by an import
  path that reads no Python package root, and `app.api.routes.items` names no
  file that way. A relative import's module is followed. colby resolves the
  absolute import from the project's package roots since #1921 (`6fe9dc12`),
  which would name fastapi-realworld's routes `POST /users/login` and the
  like; that is row 70 of the 2026-10-22 audit, a PORT row left to the next
  round (`frameworks/python.rs`).
- **`candidates` on framework references** — colby's Astro endpoint and
  Next.js route-handler references carry `candidates: [method]`, which no
  resolution step reads. A framework reference here carries none (`RefView`
  has no such field), so an unresolved one's `unresolved_refs.candidates` is
  `NULL` where colby's holds the list.
- **Dynamic-dispatch synthesis passes** — of `SYNTH_PASSES`
  (`callback-synthesizer.ts`), the field, closure-collection, emitter,
  render, JSX, Pascal form, Flutter, C++ override, interface override, Kotlin
  expect/actual, gRPC stub, MyBatis, gin middleware-chain, redux-thunk,
  object-registry, RTK Query, Pinia, Vuex, Celery, Spring event, MediatR,
  Sidekiq, Laravel event, C function-pointer, GoFrame route and Go func-field
  passes, the Vue and Angular template passes, the SvelteKit loader and page
  passes, the Expo Router return pass and the Next.js, React Router, TanStack
  Router, Vue Router and SvelteKit link passes, and the Go prerequisites
  (`go-method-contains`, `go-implements`) are ported
  (`crates/codegraph-resolve/src/synthesis/`); the others draw no edge here
  yet. colby reads no Expo Router `<Link href>`, `<Redirect href>` or
  `Stack.Screen` name either (`frameworks/expo-router.ts`, "Not covered
  yet"). The link passes keep `b635dd46`'s view of their inputs:
  #2769's escaping of a wrapper's name in the React Router prefilter, and
  #2484's `unittests/` and `*_unittest(s)/` test directories, are PORT rows
  of the 2026-10-22 audit, left to the next round. Two colby readings are
  ported as they behave: a Next.js `href={{ pathname }}` names nothing, as
  colby's sticky key, started at an offset of the whole source in a slice
  that starts there, never matches; and an Expo helper is the one function
  of its name in the navigation's file, else the project's, colby's step
  between them reading an import mapping's `resolvedPath`, which nothing
  sets. The ported passes and the merge keep `b635dd46`'s view of Jest/Vitest
  manual mocks: #2790's skip of a `__mocks__/` handler for production code
  is a PORT row of the 2026-10-22 audit, left with the rest of that row to
  the next round. The C++ override pass is `b635dd46`'s too: #2609's skip of
  a member template's explicit specialization, which shares the template's
  name, is a PORT row of the same audit, left with its extraction half. A
  TypeScript abstract method and an ArkTS interface's method
  signature are no nodes here (#2653, a PORT row of the same audit, and the
  ArkTS spec's missing `method_signature`), so a call through either reaches
  no implementation.
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
  conditions), followed by the "Dynamic-dispatch links among your symbols"
  section as `e8ddc4e2` prints it (#2748, #2751). The spine does not yet weight
  file ranking or body windows, and a flow that does not connect gets no
  boundary or polymorphic-dispatch announcement of its own; the port's boundary
  scan over the explore seeds still runs. The low-value files the links put
  last and `excludeLowValueFiles` drops are `b635dd46`'s `isLowValue` patterns;
  upstream has since moved both onto its engine-wide test predicate (#2557,
  PARTIAL in the 2026-10-22 audit). The Rust port reproduces the DETERMINISTIC
  structure — header,
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
records what colby does not or skips what colby reads. None is a Tier-3
rule: the goldens pin this port's own output, these differences included. A
difference of mechanism alone, which gives colby's result (one scan in place
of a regex compiled per name, a per-pass memo, a pattern rewritten for Rust's
regex engine), is not listed.

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
- **A path two Next.js apps serve (W7-07).** When several Next pages serve
  one path (two apps of a monorepo each with `/login`), a navigation call
  goes to its own app's page: the app whose root is the longest one above
  the calling file. Several such pages, or none, leave the call unresolved,
  and no dynamic route stands in. colby keeps one page per path, the last in
  node order; the store returns nodes in insertion order, which a sync
  changes. A path one page serves binds as in colby, wherever the call is
  (`nextjs.rs`).
- **Expo Router screens by app (W7-08).** A navigation call matches only the
  screens of its own Expo app, the longest directory above an `app/` that
  holds the calling file, exact and dynamic alike; a file under no Expo app
  is not read, and a path two screens of that app serve (`app/index.tsx`
  beside `app/(tabs)/index.tsx`) stays unresolved. colby keeps one table for
  the project, reads every file and keeps the last screen of a path in node
  order, so in a monorepo a React Router app's `navigate('/')` reached the
  Expo app's `/` screen (`expo_router.rs`).
- **A path two React Router routes of one app serve (W7-09b).** A
  navigation call matches only the routes of its own app, as in colby, and a
  path two routes of that app serve (written twice, or the bare path two
  optional-parameter routes such as `/cart/:id?` and `/cart/:item?` shorten
  to) stays unresolved, with no dynamic route standing in. colby's
  `addRouteTo` keeps the first route of a path in node order, which a sync
  changes (`react_router.rs`).
- **A React Router route file nested past 256 levels (W7-09b).** The route
  scanner's two recursive steps, a unit (a bracket group, a string or
  template, a template hole, one character) and a JSX element, share a bound
  of 256 levels. A file that goes deeper yields no routes, those read before
  that point included, and names and hands over no route table (W7-09d): a
  parent route that was never closed would leave its children's paths wrong. No file runs the call stack out; colby's scanner
  recurses without a bound. The bracket, string and wrapped-call scanners in
  `js_scan.rs` keep their stackless walks and have no bound
  (`react_router.rs`).
- **A route table whose routes' references changed (W7-09d).** The pass
  that reconciles the routes of React Router tables after `post_extract`
  replaces a route the tables still give when its name, qualified name or
  references changed: it deletes the stored route, re-resolves the edges
  that reached it and inserts the new one with its references. colby's
  `reconcileCrossFileNodes` renames such a route in place and keeps its
  references, so when a table's first reader changes, say an earlier file
  stops handing over a table its own file maps inside `<Route
  element={<Layout/>}>`, the route keeps rendering outside the layout until a
  full index (`resolver.rs`, `react_tables.rs`).
- **A React lazy loader nested past 256 levels (W7-09c).** The loader
  readers follow a loader's arrays, destructuring patterns, wrapper calls
  and default objects 256 levels deep; a loader nested deeper links nothing
  (its route renders nothing of it, and a value it is the loader of renders
  none of its module), so no loader runs the call stack out; colby's readers
  recurse without a bound (`lazy_loader.rs`).
- **A path two TanStack Router routes of one app serve (W7-10).** A
  navigation call matches only the routes of its own app, as in colby, and
  a path two routes of that app serve stays unresolved. colby's
  `addRouteTo` keeps the first route of a path in node order, which a sync
  changes (`tanstack_router.rs`).
- **A framework resolution into a module that exports nothing (W7-10).**
  Whichever strategy chose a result, a framework resolver's included, it is
  dropped when its target sits in another file's ES module that has imports
  but exports nothing (`gate_js_visibility`); colby holds only name matching to
  such a module (`isVisibleAcrossFiles`). Letting framework results through
  was measured on LibreChat and typescript-large: every edge it added ended
  in a test file or a compiler-test baseline, React tags such as `<Wrapper>`
  and `<Icon>` bound to another spec file's function and navigations bound
  to a test fixture's route. The cost: a route of a router declared in such
  a module, a `main.tsx` that builds `createBrowserRouter([...])` or a
  TanStack code route tree and renders it, is reached by no navigation from
  another file (`resolver.rs`).
- **A name or a path two Vue Router routes of one app carry (W7-11).** A
  navigation call matches only the routes of its own app, as in colby. Each
  table entry is a route of its own, listed once where it is written, its
  node named by its file, line and path, and a name or a path more than one
  entry of the app carries stays unresolved, two entries that repeat each
  other or share a line included. colby skips an entry whose path and name
  an earlier entry of its file gave, keeps the first route of a name or a
  path in node order, which a sync changes, and finds an entry's route by
  its path, so where one file declares a path twice, each name of that path
  goes to the file's last route of it. A `children` field is read only as
  an array written in place: after `children: kids`, colby's
  `indexOf('[')` reads the next array, a later entry's children, as that
  entry's too (`vue_router.rs`).
- **A Vue Router destination that is no whole literal (W7-11).** A
  navigation's destination is read only when it is whole: a string, a
  template whose holes fill whole segments, a conditional of them, or a
  local initialized to one, and of an object destination only the object's
  own `name` and `path`; a comment does not end the value (`'/' /* x */ +
  next` goes on). colby reads the literal heading an expression, so
  vue-element-admin's `replace({ path: '/redirect' + fullPath })` navigates
  to the `/redirect` frame, a hole sharing a segment (`` `/p-${id}` ``) as
  the segment, and the first `name:` or `path:` written anywhere in the
  object, inside its `query` or `params` too. The other routers read
  destinations as colby does (`route_href.rs`, `vue_router.rs`).
- **The component a Vue Router route names (W7-11).** A route renders what
  the router file's import of its identifier takes: a named import the
  module's node exported under that name, at its declaration or through an
  export list or the modules that re-export it (never a private declaration
  of the name, nor one nested in another declaration, which the module's
  text tells: the extractor marks a declaration inside an exported function
  exported; nor what `export default` declares, nor a `.vue` file's own
  export, whose template is no script), a default import the module's
  default export; a lazy `() => import(…)` renders the module's default
  export, a `.vue` file's own component. The
  route's reference to an identifier is named `component:<name>`, which
  only Vue Router answers: where the import names a module that is not
  found, or one holding no such component, the route renders nothing, and
  only a name no import reaches is looked up by name: a declaration of the
  routes file (vue-vben-admin's `const AuthPageLayout = () => import(…)`
  layout, which colby leaves unresolved), else a component. A relative
  specifier is the file it names. colby writes the bare identifier and takes the module's first
  component, else its first function whose name starts with a capital,
  whatever the import names, and otherwise any component of the
  identifier's name, which the Vue resolver and name matching bind too:
  vue-vben-admin's `component: IFrameView`, imported from a layouts module
  of three lazy constants, renders `BasicLayout` (`vue_router.rs`).
- **Angular `children` and nested constants written elsewhere (W7-13).**
  A route's `children` are read only from an array written in place, and a
  path constant's nested object only from an object written in place; colby
  takes the first `[` or `{` after the key, so `children: CHILDREN` read the
  array of a later entry, and `path: paths.account.path` whose `account`
  names another constant read the next field's object (`angular_router.rs`).
- **A cycle of Angular mounts (W7-13).** A routes file in a cycle of
  `loadChildren` mounts sits under the prefixes of its own chain up to the
  first file that repeats; colby memoizes each file's prefix in the order it
  renames the routes, so in a cycle a file's name depended on which route
  was renamed first (`angular_router.rs`).
- **An Angular path constant that aliases itself (W7-13).** A chain of local
  aliases (`const { create } = internalRoutes.accounts;`) is followed at most
  eight hops, and an alias that leads back to a chain being read reads
  nothing; colby follows aliases without a bound, so `const a = a.b;`
  recursed until the stack ran out (`angular_router.rs`).
- **A path two Angular routes of one app serve, and Angular redirects
  (W7-13).** A navigation call matches only the routes of its own app, as in
  colby, and a path two routes of that app serve stays unresolved. A
  redirect is followed through the routes and the other redirects at most
  four hops, whatever order its files are read in: a path a route serves
  keeps the route, a path whose redirects lead to two routes reaches neither,
  and so does `/` when two root `**` redirects disagree. colby keeps the
  first route of a path in node order (`addRouteTo`) and applies redirects
  in node and file order for four passes, the first to claim a path keeping
  it, so which redirect wins and how long a chain resolves depend on an
  order a sync changes (`angular_router.rs`).
- **An Angular destination (W7-13).** `'/editor/' + slug` is the literal
  and one segment only when the literal ends with `/` and one operand
  follows; a conditional or a template is read whole, as Vue Router reads
  its destinations; a relative arm of a conditional, and an arm that leaves
  the site (`//auth.example.com/login`), drop out before the arms are
  matched; `.concat` adds a segment per argument, its arguments split at
  the top level; and a route or a redirect whose path holds a constant the
  cross-file pass could not read is in no table. colby adds
  `/` and a hole to any literal head, so `'/redirect' + path` reached
  `/redirect/:x` and `'/a/' + b + '/c'` reached `/a/:x`, reads the literal
  heading a conditional or a template (a hole sharing a segment counts as
  the segment), checks only the first arm of a conditional, matches an
  absolute URL's path (`//auth.example.com/login` reached `/login`), splits
  `.concat`'s arguments at every comma, and matches an unread constant's
  placeholder as a literal segment, which any hole of a destination fits
  and a destination written the same way names (`angular_router.rs`).
- **A path two SvelteKit pages of one app serve (W7-12).** A navigation
  call matches only the pages of its own app, as in colby, and a path two
  pages of that app serve (`(a)/x/+page.svelte` beside `(b)/x/+page.svelte`,
  or the path two `[[optional]]` pages shorten to) stays unresolved, with no
  dynamic page standing in. colby's `addRouteTo` keeps the first page of a
  path in node order, which a sync changes (`sveltekit_router.rs`).
- **A SvelteKit destination that is no whole literal, or leaves the site
  (W7-12).** `goto` and `redirect` read their destination whole, as Vue
  Router does: a string, a template whose holes fill whole segments, a
  conditional of them, or, for `goto`, a local initialized to one; and an
  absolute or protocol-relative URL reaches nothing (an arm of a conditional
  that is one drops out before the other arms are matched, so
  `sso ? 'https://auth.example.com/login' : '/login'` still reaches
  `/login`). colby reads the literal heading the expression and keeps an
  absolute URL's path:
  `redirect(303, '/article/' + slug)` is read as `/article/` and reaches an
  `/article` page, a hole sharing a segment counts as the whole segment
  (`` `/profile/@${name}` `` reaches `/profile/@:user`, as
  `` `/create/preview-02${url.search}` `` reaches `/create/:item`), and
  `redirect(303, 'https://auth.example.com/login')` reaches the app's own
  `/login` (`sveltekit_router.rs`).
- **SvelteKit's `$lib` in a monorepo (W7-12).** An import of `$lib/x` is
  the file `src/lib/x` of the importing file's app, the directory above its
  first `src`, `pages`, `app` or `routes` folder, and reaches that file's own
  node, as every import edge does; colby reads `src/lib/` at the repository
  root, which in a monorepo is no app's, and takes the first node
  `getNodesInFile` returns for the file (`svelte.rs`).
- **A Svelte store subscription (W7-12).** `$store` in a component binds the
  one variable or constant named `store` its own file declares, and nothing
  else; colby takes the first variable or constant of the name anywhere in
  node order, so `$page` from `$app/stores` reached any project `page`
  (`svelte.rs`).
- **A capitalized call in a Svelte app (W7-12).** A component call binds the
  one component of its name in the calling file's directory or below, else
  the one of the project; several in the directory, or several in the
  project and none there, is none. colby takes the first component under the
  directory's path prefix, so `src/ui/List.svelte` reached a `Card` in
  `src/ui-kit/` (`svelte.rs`).
- **A tag imported from a module that exports no component under its name
  (W7-09e).** A JSX tag imported from a module of the project renders what
  the module exports under the name, as colby's (#2563); when the module
  exports no component there, the tag keeps the edge import resolution
  gives it, where colby's `jsx-render` pass draws nothing for the tag. The
  resolver chain has no way for one resolver to veto the next, and the
  static `references` edge is import resolution's (`react.rs`).
- **A framework name pattern with two candidates (W7-19a).** Express's
  middleware, controller and service patterns and Astro's component names
  resolve only through their one candidate: two declarations of a middleware
  name in the reference's file, two methods whose paths name the controller or
  the service, or two components of the name whose paths start with the
  file's directory answer nothing, and the reference goes on to import and
  name resolution as if the framework were not detected. colby takes the first
  in its store's order, so its edge follows the order the files were indexed
  in (`express.rs`, `astro.rs`).
- **Express middleware without its suffix (W7-19a).** colby's middleware
  resolution falls back to the name without a `Middleware` suffix when no
  declaration carries the whole name. The port does not: a whole name passes
  the prefilter only when a node that declares nothing carries it (an import,
  a parameter), and with the same-file rule the fallback then links that
  import's call, `corsMiddleware()` from `cors-kit`, to the file's own `cors`,
  which it is not. The outcome also reads whether another file declares the
  whole name, which a sync cannot re-read from an edge to `cors`
  (`express.rs`).
- **An Express controller's object (W7-19a).** colby's controller resolution
  also takes a method of the name from the files of nodes named
  `XController`. The port does not: with the same-file rule that finds only
  the method of an `XController` object the reference's own file declares,
  which the instance-method strategy links already, and its outcome reads
  which files declare `XController`, which a sync cannot re-read from the
  edge to the method (`express.rs`).
- **The `Astro` global (W7-19a).** Astro claims `Astro` and every name that
  starts with `Astro.`, so `Astro.redirect(…)` and `Astro.cookies.has(…)`
  resolve to their own source (1.0) whether or not any file declares
  `redirect` or `has`. colby's prefilter lets such a call through only when
  one of its names is declared somewhere, so there it resolves or stays
  unresolved by what unrelated files declare (`astro.rs`).
- **A name heuristic's tier with two candidates (W7-19b).** The pick the
  Django, Flask and FastAPI name heuristics share (#2225) resolves a tier only
  through its one candidate: two declarations of the name in the reference's
  file, its directory, a preferred folder or the project answer nothing and
  stop the search, and the reference goes on to import and name resolution
  as if the framework were not detected. colby takes the first in its store's
  order (`frameworks/name_heuristic.rs`).
- **A name heuristic's pick in another code family (W7-19b).** A pick written
  in another code family than the reference answers nothing, for a `calls`
  reference too: a Python `get_db()` in a FastAPI project whose one candidate
  is a TypeScript `get_db` is left to the other strategies. colby passes a
  framework's `calls` result across families, as a React Native or Expo
  bridge call needs, so there the guess links it; for every other kind its
  gate drops the pick and both answer nothing. A candidate of another family
  still holds its tier (`frameworks/name_heuristic.rs`).
- **A name a Python file imports (W7-19b).** The Django, Flask and FastAPI
  name heuristics guess no name the reference's file imports: the import
  names the module or the package that declares it, and import resolution
  answers there, as for a React component (#2120). netbox draws with
  `svgwrite`'s `Group`, which colby's Django resolver links to netbox's own
  `Group` model by its capitalized name; here the call resolves as it would
  without Django (`frameworks/python.rs`).
- **No name match reaches a route (W7-19b).** A route is named by the URL it
  serves, which no reference spells as a name, so the name matcher offers no
  route. A Django URL can read like one: in DRF's tests a route served at
  `path` took the imports and calls of Django's own `path`, and
  `path('not_found', not_found)` resolved its handler to the route itself,
  which now reaches the function. colby's exact-name match passes a route, an
  importable kind there, and draws those edges (`name_matcher.rs`).
- **A Spring binding's key is Spring's alone (W7-19c).** The reference an
  `@Value` or `@ConfigurationProperties` binding makes resolves only through
  the Spring resolver: one key spelt alike binds at `0.9` and of several the
  base file's (else the shortest file name's) at `0.75`; a key no config file
  holds stays unresolved, and so does one without a dot, which Spring's #1180
  gate passes over. colby lets import and name resolution outrank Spring's
  `0.75`, against its own comment that the base `application.yml` wins over
  the profile variants: a key two files hold binds, when spelt exactly, to the
  qualified-name match's first (`application-dev.yml` before
  `application.yml`), and when spelt loosely, like a key no file holds, to the
  binding node itself, which is named as its key (`frameworks/spring.rs`,
  `resolver.rs`).
- **Spring config keys of equal rank (W7-19c).** Of the keys a binding names
  that rank alike (base files, profile files with names of one length, or
  keys of the shortest length under an `@ConfigurationProperties` prefix),
  the one whose file shares the most directories with the binding's wins,
  then the first by file, line, column and id: the modules of a multi-module
  project each hold an `application.yml` and read their own (in mall, five
  `mall-portal` bindings link to `mall-portal`'s keys, not `mall-admin`'s).
  colby takes the first in its store's order, which its comment calls the
  path's (`frameworks/spring.rs`).
- **Spring's conventions stop at a pick that answers nothing (W7-19c).**
  Spring's convention rules (`…Service`, `…Repository`, `…Controller`, one
  capitalized word, `…Component` or `…Config`) go on to the next rule only
  when a rule finds no candidate. A rule whose shared pick answers nothing (a
  tier of two, a pick in another code family) ends them, so a later rule does
  not guess again with other kinds and folders. colby tries the next rule only
  when there is no candidate either, and takes the first otherwise
  (`frameworks/spring.rs`).
- **Play's nearest controller (W7-19c).** Of the classes named as a routes
  line's controller, Play takes the one whose file shares the most directories
  with the routes file, and answers nothing when two share as many, or when
  the action it picks is in another code family than the reference. colby
  takes the first class in its store's order whose file has the action: in
  play-samples, whose projects each hold a `HomeController`, that links most
  projects' routes to one project's controller (`frameworks/play.rs`).
- **Play reads only routes lines (W7-19c).** Play resolves a reference only
  when a routes file makes it, and such a reference takes no other strategy's
  answer: a line Play answers nothing for stays unresolved. colby also guesses
  a `Controller.action` that Scala or Java code writes, and lets name matching
  guess a line it leaves: in play-samples that links two Scala calls of
  `play-scala-websocket-example` to Java methods of
  `play-java-websocket-example`, and `play-scala-tls-example`'s `GET /`, whose
  three nearest `HomeController`s tie, to `play-scala-anorm-example`'s
  (`frameworks/play.rs`, `resolver.rs`).
- **Laravel and Drupal take one candidate (W7-19d).** Where colby takes the
  first of several candidates in its store's order, Laravel and Drupal take
  the only one and answer nothing for several: a `Controller@action`'s
  method among the files of the controller classes so named (those whose
  file ends with a written namespace path first), a resource route's
  controller among the classes whose file ends with its path (akaunting's
  `Common\Companies` is both `Common/Companies.php` and
  `Api/Common/Companies.php`, and its two routes files mean one each), a
  Drupal routing file's bare namespaced class, and a `hook_X`'s
  implementation. Of several Drupal controller classes of a handler's name,
  Drupal takes the only one whose file holds the method, where colby takes
  its first class and, when that file lacks the method, the class itself
  (`frameworks/laravel.rs`, `frameworks/drupal.rs`).
- **Laravel and Drupal candidates are PHP's (W7-19d).** Laravel's
  controller classes, Drupal's handler classes and its hook
  implementations are PHP classes and functions; colby takes them in any
  language, a Pascal `.inc` file's functions among the hook
  implementations (`frameworks/laravel.rs`, `frameworks/drupal.rs`).
- **Laravel's and Drupal's claims read the reference (W7-19d).** colby's
  claims take `Controller@action`, namespaced class names, `Class::method`
  and `hook_X` past the name prefilter wherever they are written, so every
  PHP `use Foo\Bar;` reaches its resolvers, and Drupal's namespaced-class
  rule links it at `0.85` to any project class `Bar`, in any namespace,
  which no sync then revisits when such a class comes or goes. Laravel
  claims only a Laravel route's references, Drupal only a routing file's
  and the `hook_X` names, and Drupal's handler rules answer only a routing
  file's references; a `use` import stays import resolution's
  (`frameworks/laravel.rs`, `frameworks/drupal.rs`, `resolver.rs`).
- **A Drupal routing handler is Drupal's alone (W7-19d).** A routing file's
  reference takes no other strategy's answer, so a handler Drupal answers
  nothing for stays unresolved. colby lets the other strategies answer it:
  without this rule instance-method matching linked four of drupal/drupal's
  `_entity_form: <entity type>.<operation>` routes to an entity class's
  method named for the operation (`menu.delete` to `Menu::delete()`, and
  `filter_format.disable`, `image_style.flush`, `entity_form_mode.delete`
  alike), where the route serves the entity type's form handler
  (`frameworks/drupal.rs`, `resolver.rs`).
- **Rails takes one candidate (W7-19e).** Where colby takes the first of
  several candidates in its store's order, Rails takes the only one and
  answers nothing for several: a route's action among the files of the
  controller classes so named (after the filter by the controller's module
  path), a model among the classes of its name in a path holding
  `app/models/`, and a controller among those in a path holding
  `controllers/` (`frameworks/rails.rs`).
- **Rails candidates are Ruby's (W7-19e).** A route's action and Rails'
  controller, model, helper, service and job candidates are Ruby methods,
  functions, classes and modules; colby takes them in any language, a
  monorepo front end's TypeScript `UsersController` among a route's
  controllers (`frameworks/rails.rs`).
- **Dart named constructors (#2380).** A named constructor, indexed as a
  method, is reached only by a call written through its class (`Foo.value(2)`).
  A call through a value of that type (a typed parameter or local, a chain's
  last link), or through a receiver the matcher reads as the class (`foo`) or
  scores by its words (`fooValue`), calls an instance or extension member of
  that name, or nothing, where colby's member lookup and receiver guesses take
  the constructor (`dart_members.rs`, `name_matcher.rs`).

### Synthesis

- **When a sync recomputes synthesis (W7-14).** A sync recomputes every
  synthesized edge whenever it changes the index, bootstraps a lane or runs
  the orphan sweep, and when the `synthesis_version` marker is missing or at
  another version.
  colby decides by what changed (`src/index.ts:985-1082,1378-1409`): a
  changed file that was a synthesis input, held a synthesized edge or
  matches `hasSynthesisPattern`, a new renderable definition, a moved route,
  workspace-package map or Python root set, or references an interrupted
  resolution left pending. A handler a name picks depends on every candidate
  of that name, and an unchanged file's base edges move when a sync
  re-resolves them, which that gate does not see; its pattern holds for
  nearly every code file anyway. The
  `synthesis_inputs` table is never written, so #2565's and #2688's filter
  of its rows has nothing to filter (`synthesis/mod.rs`,
  `codegraph-watch/src/sync.rs`).
- **The handler a name means (W7-14).** An emitter handler named `h` is the
  one function or method `h` of the registering file, else the one of the
  project, and nothing when either has several: an ambiguous name stays
  unresolved. A gin handler (W7-15c) follows the same rule among the Go
  functions and methods, the registering file being that of the name's first
  registration. colby takes the first of `getNodesByName`, the one of the file
  first by path. The passes otherwise walk colby's orders (`synthesis/stage.rs`,
  `synthesis/callback.rs`, `synthesis/go_dispatch.rs`).
- **The handler a registry maps (W7-14d).** An object-registry handler named
  `h` follows the same rule within the first kind that has a node `h`
  (function, else class or struct, else method): the one of the registry's
  file, else the one of the project, else nothing, without falling to the
  next kind. colby takes that kind's first node of `getNodesByName`
  (`synthesis/registry.rs`).
- **The thunk a dispatch names (W7-16d, OD-B14).** A redux-thunk constant's
  `dispatch(name(…))` reaches the one candidate of the first kind that has
  one: a thunk declaration, else a constant, else a function or method of the
  dispatching file, else one of the project. Several of that kind reach
  nothing, without falling to the next kind or choosing the dispatching
  file's. colby takes that kind's first node of `getNodesByName`
  (`synthesis/framework.rs`).
- **The worker a qualified name means (W7-16d, OD-B14).** A Sidekiq enqueue
  through `A::Worker` reaches the `perform` of the one class of that qualified
  name that is a worker, and nothing when several are, even when their
  `perform` is one method (two declarations of the class on one line); it then
  does not fall back to the last segment. colby takes the first such class
  (`synthesis/framework.rs`).
- **The store a dispatch names (W7-16b, hard invariant 6).** A generated RTK
  Query hook reaches the one function of its endpoint's name in its own file.
  A Pinia factory name more than one file defines is no store, a variable a
  file binds to more than one store binds none, a call reaches the one
  function of the method's name in the store's file, and a call outside every
  function comes from the file's one component. A Vuex key reaches the one
  candidate of its first tier (the store files whose path has the key's
  namespace as a segment, or for a root key the dispatching file), else the
  project's one candidate. Several of any of them reach nothing. colby takes
  the first, and the last factory and binding (`synthesis/framework.rs`).
- **The handler a GoFrame route's bare type reaches (W7-16c, OD-B16).** A
  route whose package-qualified request type no method takes falls back to
  the methods taking the bare type, leaving out one whose qualified parameter
  is another route's request type: with `cash` and `order` both declaring a
  `ListReq` route and only `order` handling one, `GET /cash/list` reaches
  nothing. A same-package (`*ListReq`) or aliased (`*sys.DeptSearchReq`)
  parameter still links. colby keeps every method of the bare type
  (`synthesis/goframe.rs`).
- **Navigation links (W7-18a, OD-B10).** A Next.js `<Link href>` names a
  page only by a destination starting with `/`, as an internal `<a href>`
  and every other router's link do; colby skips an external `<a href>`
  only, and its table reads an absolute URL's path, so a `<Link
  href="https://…/">` (mantis's MUI links) reaches the site's own `/`. An
  Expo helper's literals name the screens of the navigation's own Expo app,
  a navigation under no Expo app reads none, and the eight-screen cap and
  the memo are per helper and app, as a navigation call matches (W7-08);
  colby reads the helper against one table for the project. The files are
  read in path order, so the navigation that first reaches a helper's
  screen, whose `registeredAt` and `navMethod` the edge carries, is the same
  on every index; colby walks `getAllFiles()` in index order. A path or a
  name two routes of a link's app carry names nothing, as for the routers'
  navigation calls (`synthesis/links.rs`).
- **How the stage writes its edges (W7-18c0).** The stage keeps the row
  of each synthesized edge it makes again unchanged, deletes the stored
  ones it no longer makes or makes differently, and inserts the new ones,
  in one transaction; the rows it leaves are the ones deleting every
  synthesized edge and inserting the new list would leave. colby's
  `publish` deletes every synthesized edge and inserts every new one
  (`src/db/synthesis-stage.ts:52-89`). Where a query lists a node's edges
  of one kind by row id, a kept synthesized edge can come before a base
  edge the same sync rewrote, as base edges a sync rewrote already come
  after the ones it kept; a fresh index writes the stage's edges after
  every base edge, as colby does (`replace_synthesized_edges` in
  `codegraph-store`).
- **A Vue template's names (W7-18c).** A tag or a handler is the one node
  of its name and kinds in the template's file, else the one of the project,
  and nothing when either has several; a tag no node is named renders the
  component Nuxt auto-imports under it only when one component has that
  Nuxt name, and a handler destructured from a composable is the one member
  of its key in the composable's file. colby takes the first in node order
  at each step. A file joins nothing unless it holds exactly one component
  (`synthesis/vue.rs`).
- **Angular templates (W7-18d, OD-B18).** A `templateUrl` template the
  index holds is read as the index holds it, so one changed since it was
  indexed reads as absent until its own sync; any other is read only where
  the scan's rules admit it whatever its extension, through no symbolic
  link, no larger than `indexing.max_file_size` and never outside the
  project root, and is recorded, so that a sync that finds it reading
  differently recomputes the stage. colby reads any file at the path. An
  inline `template:` is read only when its value is a string or template
  literal; colby takes the value's first character for the quote and reads
  from there to that character's next occurrence, so `template: tag + '…' +
  tag` reads `ag + '…' + `. A link's destination is read as a navigation
  call's is (W7-13): a relative arm, or one that leaves the site, drops out
  before the arms are matched, where colby checks only the first arm
  (`synthesis/angular.rs`).
- **A SvelteKit route's page (W7-18b).** Every page the SvelteKit table
  holds calls its own file's component, a page whose path another page of
  its app serves too (route groups `(a)/x` and `(b)/x`, which SvelteKit
  rejects as a conflict). colby walks each app's paths, each holding the
  first page in node order, so only that page is joined. A page or layout
  file joins nothing, its route or its loader, unless it holds exactly one
  component; colby takes the first (`synthesis/sveltekit.rs`).
- **JSX render (W7-14e, OD-A4).** A tag renders what its static
  `references` edge reaches, a component, function, class or value, never
  a method. colby scans a file's text for `<Name` and finds the child by
  name (`jsxChild`, `declaredComponent` and its local-binding and export
  lookups). The tags are the ones the syntax tree gives in tsx and jsx
  files, so JSX in a `.js` file renders nothing here, and a type argument
  (`Array<Box>`, `useState<Box>()`) is no tag, where colby's pattern reads
  one. A tag outside every function renders from the value or the file
  the syntax tree gives it to; colby decides by line, and a line a value
  shares with another statement renders from no one (#2587). A wrapper
  (`styled(Button)`, `memo(Button)`) does not render what it wraps, colby's
  `wrapped` edges: the base graph has no edge from the wrapper to it
  (`synthesis/jsx.rs`).
- **A file that drifted since it was indexed (W7-14).** The stage reads a
  file only while its size and modification time, or its content hash, are
  the ones its file row records, the MCP engine's freshness probe; a drifted
  file reads as absent until its sync recomputes the stage, never sliced by
  its old nodes' lines. colby reads the file as it is on disk
  (`synthesis/stage.rs`). The Go steps read from that text the package an
  embedding of a type outside the index is written through, so an
  embedding in a drifted file brings no method and matches no other
  type's; colby reads it through its resolution context
  (`synthesis/go.rs`, W7-15b).
- **Mockgen's default output name (W7-15c).** A `mock_<name>.go` is a
  generated file in any directory, its file name tested alone
  (`is_mockgen_default_output`): the gRPC stub pass takes no struct of
  `svc/mock_greeter.go` for a server, and the MCP tools rank the file as
  generated without mockgen's banner. colby's `/^mock_[^/]+\.go$/` tests the
  whole path (`generated-detection.ts`), so only a root `mock_x.go` matches
  there; a nested one is generated by its banner alone, and its structs are
  candidate servers (`codegraph-core/src/file_class.rs`,
  `synthesis/go_dispatch.rs`).
- **C function pointers: a struct is one definition (W7-17a).** The
  function-pointer pass keys a registration and a dispatch by a struct
  definition and its field: each `struct` or `union` node is one, and two
  definitions with the same name and fields are two. A name means the one
  definition of that name in the project, else the one of the file's
  translation unit (the file and the files its `#include`s reach), else
  nothing; the type of a file-scope table, the one owner a field name
  falls back to, and a table entry follow the same rule, the entry's function
  being the one of its name in the table's file, else in the includer's, else
  in the project. colby keys by the name: `structLayout` keeps the last
  layout of a name, a chain walks the first with the field, and `resolveFn`
  takes the table file's first candidate, else the first
  (`synthesis/c_fnptr.rs`).
- **C function pointers: a body with conditionals (W7-17a).** A struct's
  fields are read as its definition writes them, conditionals unevaluated, an
  inline struct no node holds included (vim's `cmdname`: the whole
  `ex_cmds.h` parses to no symbol here) as the file writes it, not as the
  first includer's evaluation leaves it. From the declaration that holds a
  body's first `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else`, `#endif` or
  `#include` on, a field's position may differ between translation units,
  so a positional table entry for it is not registered; a designated
  `.field = fn` still is, and a chain never walks a field the body declares
  twice. colby reads the positions as written and walks the first field of
  the name (`synthesis/c_fnptr.rs`).
- **C function pointers: what is read (W7-17a, W7-17b).** An `#include`
  resolves to an indexed file, else, as colby's does, to a file on disk under
  the project root, but only to one the scan's rules admit whatever its
  extension (the default ignores, the root `.gitignore`, `.git/info/exclude`
  and the configured include and exclude), reached through no symbolic link
  and no larger than `indexing.max_file_size`; colby reads any file there,
  excluded, ignored or linked. So a first candidate the configuration keeps
  out is no file here, and the include resolves to the second. The macro
  patterns read `\r`, `\n` and `\r\n` as line ends, not U+2028 or U+2029,
  which JavaScript's `m` flag reads too. Every file runs through every stage,
  without colby's survival filters, which skip a file only where the stage
  would find nothing, except in two cases: an initializer whose type only a
  non-indexed include aliases, which this pass registers and colby's filter
  skips (its `aliasNames` are the indexed files' object macros), and a match
  that starts before a function and runs across a line end into its
  signature line, which shifts the matches inside its body
  (`synthesis/c_fnptr.rs`, `synthesis/inputs.rs`).
- **A Go type followed through aliases (W7-15d).** The func-field pass
  follows a type through aliases and defined types at most three deep, from
  whichever name it starts. colby keeps one answer per type name and file
  whatever the depth it reached it at (`resolveMemo`): a chain four or more
  deep resolves there when the type past the third step was resolved first,
  and that type, written in the same file, resolves to nothing once the
  chain was followed first (`synthesis/go_func_field.rs`).

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
- **Store and RTK initializers (W7-16b).** A Vue store's or an RTK Query
  api's initializer is walked as its declaration's, skipping only what became
  a node: the store call, its wrappers, `state` and an endpoint's other
  options keep their calls, as a Zustand store's do (#693, #1862). A Vuex
  module's `export default {…}` is walked as before around its collections'
  members. colby walks none of the initializer once it finds a collection or
  an endpoint, and none of the statement once the file reads like a store
  (`walker.rs`).
- **Store and RTK member names (W7-16b).** A collection member or an
  endpoint is named only by a static key (an identifier, or a string of
  `[A-Za-z0-9_$]` without its quotes) and qualified under its declaration
  (`useAuthStore::login`, `api::getPosts`); a `function*` value is a member
  too. colby names any key by its text without quotes, so a computed
  `[types.SET]` member is a node named `[types.SET]`, qualifies by the file
  and skips a `function*` value (`walker.rs`).

### Scan scope

- **`addons` skipped by default (Godot; W7-16c, OD-B17).** The default
  `indexing.ignore_dirs` adds `addons`, a Godot project's vendored editor
  plugins, to colby's default directories (with `.godot`, the engine's
  regenerated cache). Like every entry it is a directory name skipped at any
  depth in any project, so first-party code under a directory of that name is
  not indexed either: a GoFrame app's addon modules (`addons/<name>/`, which
  the GoFrame pass's addon tier tells apart) and a core directory such as
  hotgo's `api/admin/addons/`. colby indexes them. A project re-includes them
  with a custom `ignore_dirs` list in `.codegraph/config.toml`
  ([`godot.md`](../godot.md); `codegraph-core/src/config.rs`).

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
  and resolves as `strlen` (`php_scope.rs`, `import_resolver.rs`). A Laravel
  route's handler class sits at the route's column 0 under the name colby's
  extraction gives it: a simple name is read as written, through the routes
  file's `use` imports and namespace as colby reads it, and a namespaced one
  (a string handler without `@`, relative to the controllers root) reaches
  no class (`php_scope.rs`).
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
(`frameworks/nestjs.rs`), where colby's sync keeps the old name. Express's
mount prefixes need none either: every sync composes them again and renames
only Express's own routes, so another framework's route in a mounted file
keeps its name, which colby renames, and a route that no mount renames any
more gets its file's name back (`frameworks/express.rs`), where colby's sync
keeps the old prefix. FastAPI's router prefixes (`bc45e907`) need none either:
every sync composes them again and renames only FastAPI's own routes, so a
Flask route on a FastAPI decorator's line keeps its name, which colby
renames, and a route that nothing renames any more (its mount gone, its prefix
computed, its router mounted at a second path) gets its file's name back
(`frameworks/python.rs`), where colby's sync keeps the old prefix. Spring's
config keys and Play's actions have lanes of their own (W7-19c): a config file
a sync rebuilds or removes re-resolves every reference a key can bind, since a
relaxed spelling or a second profile file moves a binding without touching
its name, and a class or action a sync adds, removes or moves into another
language re-resolves the routes lines naming it (`play_routes_version`).
So do Laravel's controllers and Drupal's handlers and hooks (W7-19d): a
class or method name a sync adds to or removes from a controller file (a
path holding `Controllers`) re-resolves the Laravel route references naming
its class or file (`laravel_routes_version`), and one a sync adds to or
removes from any PHP file re-resolves the routing files' handlers naming
the file's classes, a hook function that comes or goes every `hook_X` it
answers, and a node named `hook_X` that comes or goes the resolved
references of that name (`drupal_refs_version`). So do Rails' controllers
(W7-19e): a Ruby class, method or function name a sync adds to or removes
from any file re-resolves the Rails route references naming one of the
file's classes, or naming the file as their
`app/controllers/<controller>_controller.rb` (`rails_routes_version`).
Laravel's route names need no lane: every sync recomputes them from their
qualified names, as colby's does, so a mount that goes away is undone in
both.
