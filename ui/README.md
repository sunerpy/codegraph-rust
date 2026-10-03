# ui/ — the frontend of `codegraph ui`

The browser reader for an indexed project: Svelte 5 + Vite, built into
`crates/codegraph-ui/viewer/`, embedded in the `codegraph` binary and served
over loopback by `codegraph ui`. [`../docs/ui.md`](../docs/ui.md) is the
reference for the command, its JSON API, its boundary and what it does
differently from upstream; this file is about the frontend itself.

The tree was imported from upstream colbymchenry/codegraph `v1.6.1` (MIT, see
[`LICENSE`](LICENSE)) and keeps upstream's formatting, so a later upstream sync
can still diff it. Two things are local layers on top, and
[`../docs/upstream-sync/UPSTREAM.md`](../docs/upstream-sync/UPSTREAM.md) records
both for that sync to re-apply:

- the look: direction D
  ([`../docs/design/viewer-d.md`](../docs/design/viewer-d.md)), with a dark and
  a light theme;
- this README.

Upstream also packages these components as `@colbymchenry/codegraph-ui` from
`src/index.ts`. That entry is kept, but this repository builds only the app.

## Build

The toolchain is pinned in `package.json` and `package-lock.json`, and `npm ci`
is the only install path. From the repository root:

```bash
make ui          # npm ci, then vite build into crates/codegraph-ui/viewer/
make ui-check    # npm ci, svelte-check, vitest, a fresh build, and a byte check
```

Inside `ui/`:

```bash
npm run check    # svelte-check
npm test         # vitest: the suites in tests/
npm run dev      # Vite on 127.0.0.1:5174, with no API behind it
```

The bundle is committed. The crate's `build.rs` embeds every file under
`crates/codegraph-ui/viewer/`, so `cargo build` and `cargo install --git` never
need Node. `make ui-check` is the CI `UI` job and part of `make pre-ci`. It
fails when the committed bundle is not byte-for-byte a fresh build, so a change
to `src/` lands together with the bundle `make ui` rebuilt from it.

`npm run dev` serves the app on its own, so every `/api/…` request it makes
fails. To work against a real index, add a `server.proxy` for `/api` to your
local `vite.config.ts` that points at a running
`CODEGRAPH_UI=1 codegraph ui --no-open --port <N>`. Or rebuild with `make ui`
and run the binary.

## Layout

```
src/
  main.ts                 fonts, the stored theme, mounts App into index.html's #app
  index.ts                upstream's library entry; kept, not built here
  app.css                 the shell grid and the primitives (islands, pills, buttons, …)
  lib/theme.css           the design tokens: Daylight (light) and Nebula (dark)
  lib/theme-choice.svelte.ts  System / Dark / Light, stored and applied as data-theme
  lib/icons.ts            Lucide icon geometry (ISC), drawn as CSS masks by Icon
  lib/adapter.ts          GraphAdapter, createHttpAdapter, the registry
  lib/wire.ts             every Wire* payload shape — types only, no runtime
  lib/api.ts              the screens' calls, one line each, over the adapter
  lib/navigation.ts       href builders + navigate, behind a driver
  lib/destinations.ts     what the nav rail, the phone tab bar and the More sheet offer
  App.svelte              nav rail / command bar / trail ribbon / main, global keys
  lib/router.svelte.ts    hash router: #/s/<id>, #/file/<path>, #/map, #/flow, #/entry, …
  lib/trail.svelte.ts     the walked path; mirrored into the `t` query param
  lib/trails.svelte.ts    saved trails: one shared fetch, and the two writes
  lib/trails-model.ts     what a saved trail's row says, incl. its decay (pure)
  lib/kinds.ts            kind glyph letters
  lib/symbol-model.ts     the Symbol view's rails and callee rows (pure)
  lib/map-model.ts        the Map's deterministic layered layout (pure)
  lib/flow-model.ts       the Flow strip's card/link geometry + the end cap — a DAG (pure)
  lib/filecode-model.ts   the whole-file view: fixed line height, arcs, paging (pure)
  lib/hierarchy-model.ts  the type hierarchy's tree geometry (pure)
  lib/entry-model.ts      the entry-points panel: rows, file groups, flow arming (pure)
  lib/screens-model.ts    the Screens view: layering by distance from the entry, edge labels, pill lanes (pure)
  lib/export-svg.ts       the Flow strip and the Map as a standalone SVG (pure)
  lib/export-image.ts     rasterising that SVG to PNG, clipboard and download
  lib/live.svelte.ts      /api/events: two counters every screen refreshes from
  lib/toast.svelte.ts     the one transient note ("Index updated · reloaded")
  components/             NavRail, TopBar (the command bar), TrailBar, PhoneTabBar, MoreSheet, ThemeSwitch, SearchPalette, SavedTrails, KindGlyph, Icon, DriftBanner, ErrorCard, Toast, ExportButtons, map/, flow/, symbol/, file/, entry/, screens/, steps/
  views/                  one component per route
tests/                    the vitest suites, one per model or behaviour
```

Fonts (Inter and JetBrains Mono, both variable) are vendored through
`@fontsource-variable` and emitted with the bundle's assets — never inlined, so
the server's `font-src 'self'` policy holds: a local reader must work offline
and must not announce the project to a font CDN.

## The adapter is the only way data arrives

```ts
interface GraphAdapter {
  stats(signal?): Promise<WireStats>;
  search(query, opts?, signal?): Promise<WireSearch>;
  node(id, signal?): Promise<WireSymbolPayload>;
  nodes(ids, signal?): Promise<WireNodeRefs>;
  source(request, signal?): Promise<WireSource>;
  file(path, signal?): Promise<WireFilePayload>;
  fileCode(path, signal?): Promise<WireFileCodePayload>;
  flow(request, signal?): Promise<WireFlowPayload>;
  map(request?, signal?): Promise<WireMapPayload>;
  screens(signal?): Promise<WireScreensPayload>;
  routes(request?, signal?): Promise<WireRoutes>;
  entryPoints(request?, signal?): Promise<WireEntryPoints>;
  deadCode(request?, signal?): Promise<WireDeadCode>;
  trails(signal?): Promise<WireTrails>;

  // Optional.
  steps?(request, signal?): Promise<WireStepsPayload>;
  saveTrail?(request, signal?): Promise<WireTrails>;
  deleteTrail?(id, signal?): Promise<WireTrails>;
  events?(handlers): () => void;   // the live channel
}
```

The shapes are what `crates/codegraph-ui/src/api/` serialises, upstream's wire
format kept as it is, and they live in `src/lib/wire.ts`, which has no imports
and no runtime. The app runs every screen on `createHttpAdapter()`, the loopback
JSON API.

`steps` is optional, and `createHttpAdapter()` leaves it out, because the Rust
server does not build Steps yet. The Steps view therefore says it cannot draw
steps.

`events` is optional. Omit it and nothing connects and nothing polls; a host
that learns about a sync some other way calls `live.signal('index')` instead,
which is the same code path the stream uses.

`saveTrail` / `deleteTrail` are optional for a different reason: they are the
only methods in the interface that CHANGE anything, and a reader must be able to
render without inheriting a write it never asked for. Omit them and `TrailBar`
grows no Save button and `SavedTrails` says the host does not store them — the
same thing it does when `trails()` answers `readOnly: true`, which is what
`codegraph ui --read-only` does. `trails()` itself is required: a host with
nowhere to keep them answers an empty read-only list, so the screen is
explained rather than silently missing.

`TypeHierarchy` is the one screen that takes its data as a prop rather than
asking the adapter: it is part of `SymbolView`'s payload (`/api/node`'s
`hierarchy`), so whoever already holds a `WireSymbolPayload` can render the tree
without a second read.

### Three things that will bite

1. **The tokens are the only colours.** Every component paints from
   `lib/theme.css`. Daylight sits on the bare `:root`; Nebula sits under
   `prefers-color-scheme: dark` and `[data-theme='dark']`. Both sets use the
   same names, so a new colour is a token in both. `tests/theme-contrast.test.ts`
   fails when a declared text/surface pair drops under 4.5:1, when the copies of
   a set drift apart, or when the SVG export's palettes stop matching them.
2. **The adapter and the navigation driver are module-level, not context.** The
   pure model modules are plain TypeScript and cannot read a component's
   context, so one page reads one project. `<CodegraphUi>` installs them during
   initialisation, once — swapping projects means re-mounting the subtree
   (`{#key project}`), not swapping the prop.
3. **Geometry is not themable.** 44px callee rows, the 288/320px rails, the 20px
   code line: the Symbol view measures these against each other to put a callee
   row beside the line that calls it.

### Navigation

Every link the components build goes through a `NavigationDriver`
(`src/lib/navigation.ts`). The default is the viewer's own hash space
(`#/s/<id>`); a host installs one that addresses its app instead, and the rails,
breadcrumbs, chips and cards follow. They are hrefs rather than click handlers
because middle-click, cmd-click and "copy link address" are how people read
code.

The app's half — parsing the hash, holding the live route — is
`src/lib/router.svelte.ts`. It attaches `hashchange`/`popstate` listeners at
module scope, which is why upstream keeps it out of the component library.

## Export

The Flow strip's header and the Map's side panel carry **Copy image** (a PNG on
the clipboard) and **Download SVG** (a file for a README). Both render the
theme on screen (light when the exporter is called without one), with 24px of
paper around the drawing, a caption naming the path or the root, and a
"CodeGraph" mark in the corner.

`export-svg.ts` **serialises the layout object**; it does not scrape the DOM.
`buildFlowLayout` and `buildMapLayout` already compute every rectangle, port and
curve before a component renders, so the image and the screen come from one
piece of arithmetic and cannot disagree — and the exporter is a pure function
that a test can run with no browser at all. The output is presentation-only SVG
(no script, no `foreignObject`, no external reference), which is what GitHub
will render in a README.

Fonts travel as `font-family` stacks rather than embedded bytes. An SVG loaded
as an image may not fetch a webfont, so a raster falls back to the platform's
own monospace; every fallback in the stack advances at ~0.6em like JetBrains
Mono, so the code grid survives and only the letterforms change.

## Routes

| hash | view |
|---|---|
| `#/` | Start: the project and what its index holds |
| `#/s/<id>?hl=<line>&t=<trail>` | symbol view |
| `#/file/<path>?hl=<line>` | file view — outline in source order |
| `#/file/<path>?src=1` | file view — the whole file's source, with ports and call arcs |
| `#/map?root=&depth=&tests=1` | module map |
| `#/flow?from=&to=` | flow strip — the call path between two symbols |
| `#/flow?symbols=a,b,c` | flow strip — `codegraph_explore`'s own question |
| `#/flow?t=<trail>` | flow strip — the trail you walked, read as a flow |
| `#/entry` | entry points — routes, files that run something, tests, hubs |
| `#/dead?exported=1` | dead code — `exported=1` widens it to exported symbols |
| `#/screens` | screens — the app's screens and the transitions between them |
| `#/steps?anchor=&symbol=&depth=&through=1&view=order` | steps — what happens from a screen or a symbol |

## Entry points

`#/entry` draws `/api/entrypoints` as file groups, reusing the Symbol view's
`.filegroup` / `.row` shapes rather than inventing a second visual language for
"a list of code, grouped by where it lives". Three things about it are decisions,
not accidents:

- **Routes group by where the URL is REGISTERED, not where it is served.** A
  router file is the shape a reader already has in mind; handlers scatter across
  a package. The payload carries both, and the row's meta line names the handler
  and its `file:line`.
- **A row offers a flow only if it names a callable symbol.** `/api/flow`
  searches the graph by NAME, and a file has none the path finder can look up —
  so route and hub rows carry a `Flow ›` chip and file and test rows do not. A
  chip that always failed would be worse than no chip.
- **No empty Routes box.** A project with fewer than three resolvable routes is
  not a routed app, and the section is absent rather than empty; the panel falls
  back to the files that run something and the tests that exercise them.

`buildEntryPanel` is pure and keeps `panel.rows` exactly equal to the sections it
draws, the same identity the search palette rests its keyboard on.

## Screens

`#/screens` draws `/api/screens` — the app as its user meets it: one box per
screen, an arrow for every way of getting from one to another, and on each
arrow the condition under which it happens. This server answers upstream's "no
screen navigation" result until the navigation resolvers are ported, so here
the canvas is exercised by its suite rather than by an index.

The canvas is the Map's layout engine (`buildMapLayout`) driven by
`screens-model.ts` with three options the Map never sets, because a screens
graph differs from a module graph in one way that shapes the whole picture: it
is full of cycles. Every screen returns to Home.

- **Layering is distance from the entry screen**, measured over every
  transition — not longest path over a two-cycle-broken set. Shared chrome (a
  top bar rendered on ten screens) hangs one row above the shallowest screen it
  opens, so what it opens is placed by the entry and never dragged up beside
  Home; what only chrome reaches is seeded from the chrome. Whatever nothing
  reaches sits in a band at the bottom, one empty row below the rest.
- **Ports are directional.** A transition down the picture leaves the bottom
  of its box and arrives at the top of the other, as on the Map; a return
  leaves the **top** of its source and arrives at the **bottom** of its target,
  so it is drawn around the boxes rather than through them; a transition
  between two screens on one row arches over the row, top to top. A hub widens
  so its ports are at least 12px apart (`portPitch`), and rows are 116px apart
  instead of the Map's 44 (`layerGap`), because the edges here carry labels.
- **Lines fan out instead of stacking.** Drawn through one midpoint, every
  line between two rows crosses that height at its middle, and a line to a
  screen far to the side is nearly horizontal there — a hub's lines run stacked
  within a few pixels for hundreds, and no pointer can pick one.
  `trackedCurves` gives each line in a fan (the lines leaving one side of one
  box towards one side) a track of its own: the farthest-reaching runs nearest
  the hub's row, the next a track further out, nested in port order so no two
  lines of a fan cross; a line spanning several rows keeps its track beside
  its fan and drops the rest of the way vertically; a wider level arch rises
  higher than a narrower one. There are no hit paths: hovering the canvas
  means the line **nearest** the pointer (`nearestEdge`, within 10 screen
  pixels), so moving a few pixels moves to the next line, predictably. Zoom
  runs to 3× — the honest spacing control, since it scales lines and text
  together.
- **Labels are placed by the model, at the far end.** At rest the picture is
  boxes and lines. Selecting a screen labels each of its transitions with the
  innermost condition — the clause decided at the navigation call
  (`…guide.dontShowAgain.captureGuide`); the first thirty characters of the
  whole chain are usually shared with a sibling — prefixed `→` leaving the
  selected screen or `←` arriving at it. `placeLabels` puts every pill beside
  the screen at the *other* end of its line, where the lines are apart (beside
  the selected screen fifteen of them share one box's width), in the first of
  five lanes walking away from that box in which it overlaps nothing, centred
  on its own curve at that height. A pill that fits nowhere is counted and the
  panel says so. Pills are HTML in Svelte Flow's edge-label layer, above every
  stroke; the selection alone decides where they go, so hovering never reflows
  them.
- **The panel and the picture point at each other.** A row under the pointer
  lights its line and prints the whole condition on it; a line under the
  pointer tints its row.

The geometry — ports, curves, pill lanes — is arithmetic in `screens-model.ts`
and `map-model.ts`, tested without a browser in
`tests/ui-screens-model.test.ts`.

## Where the graph stops

A flow that does not reach everything it was asked about carries a
`boundary` on the wire, and `buildFlowLayout` turns it into an extra 240px node
one column past the symbol the path stopped at, joined by a dotted `2 4` link
labelled "end of static path" that deliberately has **no arrowhead** — an arrow
would point at a continuation, and the absence of one is the finding.

Two rules hold it together:

- **The cap's height is arithmetic, like a card's.** `endCapText()` builds every
  sentence the cap shows and `endCapHeight()` measures them; the component then
  renders exactly what was measured. Change the wording in one and the other
  moves with it — they are the same function read twice.
- **One cap per stopping symbol, not per flow.** Two paths that run out at the
  same place ran out for the same reason, and two caps side by side would read
  as two different findings.

The verdict itself is not computed here. The server's `find_dynamic_boundaries`
(`crates/codegraph-ui/src/api/boundary.rs`) runs `scan_dynamic_dispatch` from
`codegraph_mcp::dynamic_boundaries`, the same detector `codegraph_explore`
announces boundaries with.

## The type hierarchy

A class, interface, struct, trait or enum carries a `hierarchy` on its
`/api/node` payload: ancestors up, subtypes down, and the fan an interface call
dispatches into. `buildHierarchyModel` turns it into a tree whose geometry is
arithmetic — 26px rows (`HIER_ROW_H`), 28px of indent per descendant level
(`HIER_INDENT`), connectors computed from those two numbers. Nothing is
measured; the same payload always draws the same picture.

The details worth knowing before changing it:

- **`extends` is solid, `implements` dashed `4 3`, a synthesized edge dashed
  `6 3`** with a `via <mechanism>` pill. The Rust index synthesizes no
  dynamic-dispatch edges, so this server never sends that last kind, and a Go
  interface's implicit implementations are not part of its tree.
- **Overrides on the members outline are a NAME match**, not an `overrides`
  edge (nothing in the engine emits one). They are matched against the nearest
  ancestor that declares the name and are blind to signatures, and the tooltip
  says which claim was actually checked.
- **The fold trims the deepest end**, because the walk is breadth-first: a
  reader looking at an interface gets every direct implementation before any
  subclass of one appears at all.

The walk is not computed here: it is `build_type_hierarchy` in
`crates/codegraph-graph/src/hierarchy.rs`. Upstream's `countImplementers`, which
upstream's `codegraph_explore` prints when it announces an interface dispatch,
is not ported.

## Live updates

The viewer never polls. `lib/live.svelte.ts` holds one `EventSource` on
`/api/events` for the life of the page and exposes two counters:

- **`indexTick`** — the graph moved (somebody synced). Every screen refetches:
  a rail is an answer about the whole graph, and a symbol gains a caller when
  some *other* file is edited, so filtering by the focused file would leave the
  rails quietly wrong. One request per sync.
- **`diskTick`** — source files changed on disk and the index has not caught up.
  Only the screen showing one of those files reacts, and what it does is draw a
  drift banner.

`liveRefresh(file, refresh)` is the three lines of bookkeeping that turns a
counter into a single call; the Map and the Flow strip instead read
`live.indexTick` straight inside the effect that already fetches them.

Reconnection is ours, not `EventSource`'s: each failure closes the stream and
schedules ONE retry on a backoff that ends after eight attempts (~90 s), at
which point the command bar says "Not live" and nothing more is requested until
the tab is focused again. A `degraded` event — the server's watcher gave up — is
shown the same way and never answered with a poll.

Node ids and file paths are encoded per slash-separated segment, so
`#/file/crates/codegraph-ui/src/api/mod.rs` stays readable and still round-trips
a segment containing a reserved character. Build hashes with `symbolHref()` /
`fileHref()` / `mapHref()` / `flowHref()` rather than by hand.
