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
NestJS, React, Vue, Godot, Tauri, Nuxt, React Router, TanStack Router, Next.js,
Svelte, SvelteKit, Vue Router, Angular Router and Expo Router
(`crates/codegraph-resolve/src/frameworks/`), detected per-project by
`detect_frameworks`. The other colby resolvers stay deferred. The following
colby behaviors are intentionally NOT ported in v1:

- **The other framework-specific resolvers** — every concrete `FrameworkResolver`
  in `reference/colby/src/resolution/frameworks/**` EXCEPT those above:
  Spring/Java, Drupal `routing.yml`, Express, React Native / Expo / Fabric
  bridges, Swift↔ObjC bridging, the Go-module / Python / Ruby / C# / Play route
  extractors, and the Astro resolver
  (`frameworks/astro.ts`: `astro:*` module imports + the `Astro` global resolved
  as framework-provided, and `src/pages` file-based route-node mapping). These
  are an additive heuristic layer; on a project where none of them is
  detected the orchestrator holds an empty resolver list, so behavior matches
  colby with zero frameworks detected. The Astro
  EXTRACTION half (frontmatter + `<script>` delegation, template `{call(...)}`
  and `<Component>` refs) IS ported (`crates/codegraph-extract/src/embedded/astro.rs`,
  #768); only its framework-resolution half is deferred. Re-enable any of the
  deferred resolvers by adding it to `frameworks/` and the `detect_frameworks`
  registry — no orchestrator change required.
- **Router link synthesis** — `next-router-synthesizer.ts` (a Next.js
  `<Link href>` or internal `<a href>`), `react-router-synthesizer.ts`
  (React Router's `<Link to>`, `<NavLink to>` and `<Navigate to>`), Expo
  Router's `<Link href>`,
  `<Redirect href>` and `Stack.Screen` names, and the screens a helper's
  returned href names (`router.replace(await postLoginRoute())`): JSX
  attributes and return values, not calls, so they draw no `navigates` edge
  here; the routers bind navigation calls only.
- **Dynamic-dispatch synthesis passes** — of `SYNTH_PASSES`
  (`callback-synthesizer.ts`), the field, closure-collection, emitter,
  render, JSX, Pascal form, Flutter, C++ override, interface override, Kotlin
  expect/actual, MyBatis and object-registry passes and the Go prerequisites
  (`go-method-contains`, `go-implements`) are ported
  (`crates/codegraph-resolve/src/synthesis/`); the others draw no edge here
  yet. The ported passes and the merge keep `b635dd46`'s view of Jest/Vitest
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
  unresolved. colby takes the first of `getNodesByName`, the one of the file
  first by path. The passes otherwise walk colby's orders (`synthesis/stage.rs`,
  `synthesis/callback.rs`).
- **The handler a registry maps (W7-14d).** An object-registry handler named
  `h` follows the same rule within the first kind that has a node `h`
  (function, else class or struct, else method): the one of the registry's
  file, else the one of the project, else nothing, without falling to the
  next kind. colby takes that kind's first node of `getNodesByName`
  (`synthesis/registry.rs`).
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
