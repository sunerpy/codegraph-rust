# Browser viewer (`codegraph ui`)

`codegraph ui` opens a local, read-only reader of a project's existing index in
the browser: a symbol with its callers, source and callees side by side, a file's
outline and imports, the call path between two symbols, the repository as a map
of modules, a type's hierarchy, the code nothing reaches, the places to start
reading an unfamiliar project, an app's screens and the ways between them, and
what happens from a screen, an endpoint or a function. It is a Rust port of the
upstream colbymchenry/codegraph `v1.6.1` viewer, with Steps, Screens and the
conditions on call sites taken from upstream `e8ddc4e2`: the frontend is
upstream's `ui/` (Svelte 5 + Vite, MIT, see [`ui/LICENSE`](../ui/LICENSE)),
restyled to the selected design ([`design/viewer-d.md`](design/viewer-d.md),
direction D) with a dark and a light theme, and the JSON API behind it is the
`codegraph-ui` crate.

The viewer is a **preview**, gated exactly as upstream gates it: unless
`CODEGRAPH_UI=1` is set, `ui`, its alias `web`, `help ui` and `ui --help` are
refused before any startup work, and the command is left out of `--help`.

```bash
CODEGRAPH_UI=1 codegraph ui                    # the indexed project you are standing in
CODEGRAPH_UI=1 codegraph ui ~/code/my-app      # a specific indexed project
CODEGRAPH_UI=1 codegraph ui --port 8080        # one specific port (fails if it is taken)
CODEGRAPH_UI=1 codegraph ui --no-open          # print the URL; do not open a browser
CODEGRAPH_UI=1 codegraph ui --read-only        # refuse every write, saved trails included
```

`[path]` walks up to the nearest directory with an index, like the lifecycle
commands. Without `--port` the viewer takes 4747, or the next free one of the 20
ports after it. It walks past a port in use, and past one the system refuses
where the next may be allowed: on Windows any refused port (another program
holds it exclusively, or it is in a reserved range, which
`netsh int ipv4 show excludedportrange protocol=tcp` lists), elsewhere one from
1024 up; below 1024 off Windows the refusal is the privileged-port rule and the
walk stops there. An explicit `--port` never moves, and its error names the
platform's reason. `CODEGRAPH_BROWSER=<command>`
picks the browser that opens, `CODEGRAPH_BROWSER=none` (or `0`, `false`, `off`)
opens none. `Ctrl+C` or `SIGTERM` stops it: open streams end first, then the
server drains.

## What it reads, and what it writes

The viewer opens an index that already exists — it never creates, migrates,
indexes or syncs one. Every API request opens the published index read-only
(`Store::open_for_read`, the same leased open the MCP engine uses) and drops it
when it answers, so a rebuild between two requests is simply picked up by the
next one. With no index it answers `no-index` (HTTP 503) with the command that
fixes it; an index this binary cannot read (an older extraction, a build in
progress, a newer format) is `index-unusable` with its own remedy.

The one thing it writes is a **saved trail**: a walk through the graph that a
reader named and kept. Trails are JSON files under the index root it resolved,
`<index root>/ui/trails/<slug>.json` — `.codegraph/ui/trails/` by default, and
under the override when `CODEGRAPH_DIR` selects another index directory. A save
writes a temp file beside the target and renames it. A hop is stored by what it
is (qualified name, kind, file) with its node id only as a fast path, and every
hop is re-resolved when the list is read: `ok`, `moved`, `ambiguous` (a labelled
best guess) or `missing`. `--read-only` refuses saving and deleting with HTTP 403
and still lists what is there.

## Boundary

The server binds `127.0.0.1` only and refuses, before anything else is read:

- a method other than `GET`, `HEAD`, `POST`, `DELETE` (405);
- a `Host` that is not `localhost`, `127.0.0.1` or `[::1]` on its own port — the
  DNS-rebinding guard (403);
- an `Origin` that is not the viewer's own (403); no `Access-Control-*` header is
  ever sent;
- a write without the `x-codegraph-ui: 1` header or with a non-JSON body type
  (403), a write outside `/api/` (405), or a body over 64 KB (400);
- a raw path with a `..` segment however it is encoded, a control byte, a
  backslash or a malformed escape (404).

Every file the API reads goes through one chokepoint that refuses an absolute
path, a traversal and a symlink that resolves outside the project, and only opens
files the index names. Sensitive system and home directories are refused as a
project root. Every response carries `nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: no-referrer` and a content security policy that allows only
the viewer's own scripts and connections.

## Live updates

`GET /api/events` is a server-sent event stream: `hello` (the index revision the
client is synchronised against, and which observers came up), `changed` (source
files that changed on disk, root-relative logical paths, at most 200 named with
the real total; `scan: true` when the change could not be described file by
file), `index` (another process finished writing the graph, with the files it
re-indexed) and `degraded` (live watching stopped for good). A comment frame
every 25 seconds keeps the connection alive; nothing polls.

The source half is the engine's own watcher in observe-only mode
(`WatchOptions::observe_only`): the same per-platform registration, indexing
scope, symlink mapping, debounce and degrade latch as `serve`'s watcher, with
its two sync closures replaced by reporters — it never writes the index.
`CODEGRAPH_NO_WATCH`, `watch.enabled = false` and a too-broad root turn it off,
and `hello` says so. The index half is one non-recursive watch on the index
directory: a settled write (400 ms of quiet, at most 3 s) is followed by one
revision query, and only a revision that moved becomes an event. Both exist only
while a browser is subscribed.

A file that changed since it was indexed is **drifted**: size and millisecond
mtime first, then a sha256 when those disagree, so a rewrite of identical bytes
is not drift. Source is then omitted rather than sliced at line numbers that no
longer match (`/api/source?ondrift=current` serves the current bytes, flagged).

## API

All answers are JSON; failures are `{error, code, hint?}` with `bad-request`
(400), `refused` (403), `not-found` (404), `no-index` / `index-unusable` (503) or
`internal` (500). Parameters are validated, never clamped.

| Route                                                         | Answers                                                                                       |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `GET /api`                                                    | the endpoints this server answers                                                             |
| `GET /api/stats`                                              | index state, graph counts, detected frameworks, thresholds, blast-radius scale                |
| `GET /api/search?q=&limit=`                                   | ranked symbol search with the `kind:` / `lang:` / `path:` / `name:` filters                   |
| `GET /api/node/<id>`                                          | one symbol: callers, callees, types used, members, type hierarchy, tests, outside refs, blast |
| `GET /api/nodes?id=…`                                         | names and locations for up to 60 ids, in the order asked                                      |
| `GET /api/source?file=&from=&to=&ondrift=`                    | verbatim lines with syntax classes, or a drift answer                                         |
| `GET /api/file/<path>`                                        | a file's outline, imports in and out, dependencies, unresolved imports                        |
| `GET /api/filecode/<path>`                                    | a file line by line: call sites per calling symbol, references that leave the index           |
| `GET /api/routes?limit=`                                      | URL → handler map, when at least three production routes exist                                |
| `GET /api/entrypoints?limit=&routes=`                         | routes, files that run something at their top level, the widest-reaching tests, hubs          |
| `GET /api/map?root=&depth=`                                   | the repository by module: links with their declared subset, file-level cycles                 |
| `GET /api/deadcode?limit=&kinds=&exported=&tests=&generated=` | what nothing reaches, grouped by file, with every exclusion counted                           |
| `GET /api/flow?from=&to=` · `?symbols=` · `?hop=…`            | the call path as cards with each hop's condition, and where the static graph stops and why    |
| `GET /api/screens`                                            | screens and the ways between them, each with the conditions it runs under                     |
| `GET /api/steps?anchor=&symbol=&depth=&limit=&through=`       | what happens from a screen, an endpoint or a symbol, as typed steps and in the code's order   |
| `GET /api/events`                                             | the live channel above                                                                        |
| `GET/POST /api/trails`, `DELETE /api/trails/<id>`             | saved trails, re-resolved; the one write                                                      |

A call's conditions (`when`) are read from the caller's current source with the
extractor's grammars, never stored. The Symbol view's caller and callee rows
carry them, a Flow hop carries them on its edge and in its label
(`calls · when !busy && ready`), and every Screens and Steps site carries the
conditions it runs under. A file that changed since it was indexed gives none,
and so does a language without condition rules: the rules cover TypeScript,
JavaScript, Swift, Python, Java, Kotlin, C#, Go, C, C++ and Objective-C. A
Symbol request reads at most 24 files and 400 sites, the focal file first, and
starts no new file after 40 ms.

Screens draws an app as its user meets it. A screen is a route whose name starts
with `/` (a Nuxt `server/api/` file is not one), and a way between two screens is
a `navigates` edge a router resolver drew, filed on the screens whose code
reaches it: from the code that navigates, the walk climbs back through calls,
renders, function references and containing components to the component a
route renders, or a layout the route renders inside, and never across a hop
into another execution context (a cross-tier channel, a native event). The
chain becomes the way's `via` and its call sites' conditions its `when`. A
layout's ways are drawn on every screen inside it, a component three or more
screens render is drawn once as shared chrome, and a navigation no screen
reaches is drawn from the function that holds it. With no screen route, or no
navigation into one, the answer is `routed: false`.

Steps walks forward from its anchor, a node id (`anchor`) or a name (`symbol`):
of several symbols with the name it takes a route, then a component, a
function, a method, a class, a constant and a variable, and lists up to eight
others as `ambiguous`. What it meets is typed: another screen; a handler, with
what fires it (a prop, a template binding, a decorator, a request, a queue
consumer); a call or an event that crosses a language, a tier or a channel
(`bridge`, `event`); a store action; and a call that leaves the index (`effect`:
the network, the database, a reply with its status, a queue, email, storage and
the rest). Plain calls fold into the link between two steps, listed as its
`via`. `depth` (1 to 14, 8 by default) is how many steps it walks, `limit` (20 to
400, 120 by default) how many it draws, and `through=1` enters the screens and
endpoints it reaches instead of drawing them as boundaries; `/api` lists the
first four, as upstream does. A helper with forty or more incoming edges and a
component five or more others render (a file rendering at its top level is no
parent) are dead ends, counted in `truncated`, and a walk from production code
never enters a test file. `program` is the anchor's body in the code's order,
with its forks, loops and early ends, for the _in order_ reading, or `null`
when the anchor has no body. A request without an anchor, or with a `depth` or
`limit` out of range, is a `bad-request`; an id or a name the index does not
hold is `not-found`.

The entry points' hubs leave out symbols in test files and in generated files
(the indexed flag or the path convention), so a vendored bundle's one-letter
functions, depended on by the bundle alone, never top the list.

The files that run something do not count the calls inside a Swift file's
top-level `#Preview { … }` macros, trailing closures included: a preview is
Xcode's canvas, so a view whose only top-level calls are previews leaves the
list, and a file with other top-level calls is ranked on those.

A directed Flow (`from` and `to`) looks its two ends up by the names as
written, a symbol's own name: a two-letter method, a Ruby `valid?` or `save!` and
an Objective-C selector such as `initWithFrame:` are found as any other name. A
`symbols` Flow takes the symbol-shaped words of its query, and of two routes of
one length to a symbol it keeps the one that ends on fewer unnamed symbols, then
the one through more of the named ones, not the one the index lists first.

The routes list, and the routes among the entry points, hold one row per
route: the handler a resolver bound to it (a screen's component, such as a Nuxt
page's own, counts, upstream #2267), else `inline handler` standing for a
handler written at the registration, which opens on the route itself and offers
no Flow. A symbol's members list each member once, never the symbol itself or a
file.

The Map, asked for no `root`, opens on the top directory holding a clear
majority of the non-test symbols, files loose in the repository root counted in
the whole. When that directory draws one box at every depth (Express's `lib/`,
an R package's `R/`) and the whole repository draws more, it opens on the whole
repository instead. A folder with exactly one subfolder and no file of its own,
read from every indexed file with tests included, is not a level: a Maven or
Gradle project's `src/main/java/com/acme/app/` counts as one, so the map opens
on the packages under it. A box's `label` writes such a chain of three or more
folders `first/…/last` (`src/main/…/app/owner`); the `id`, the box's tooltip
and the side panel keep the whole path, and every other label is the id.

The Dead code list leaves off a symbol carrying a decorator, including one
imported from outside the index (`@HostListener`, `@Cron`), which leaves only
the resolver's unresolved `decorates` row. It also leaves off a TypeScript hook
its framework calls by name, counted as `hooks`: a method on a decorated class
that skips the `implements` clause (`ngOnInit` without `implements OnInit`),
when the classes that do write an interface from outside the index out, more
than half of them decorated, all have the method, declared or inherited, and at
least two and more than half of them declare it. Which names are hooks comes
from those classes, not from a list, so an unused helper on the same component
stays listed. Java, C# and Kotlin check the interface itself, so the rule is
TypeScript's only.

## Differences from upstream

The wire shapes, limits, error codes and refusals follow upstream `v1.6.1`, and
those of Steps, Screens and the conditions follow upstream `e8ddc4e2`. Where the
Rust graph holds different facts, the viewer shows what this index holds:

- Steps reads the name a call was written with (`prisma.user.create`, where the
  edge says `create`) from the reference that resolved at the call's site.
  Upstream keeps it on the `calls` edge (`metadata.refName`); this index keeps it
  on the resolved reference, and the edge carries no copy. Of several names
  resolved at one site the walk takes the one whose last segment is the target's
  name; with none, or more than one, the target's name stands in, as upstream's
  does for an edge without `refName`.
- Screens and Steps visit the graph in a total order: edges by line and target,
  then kind and column; a layer of callers target by target, each target's
  edges by source, line, column and kind; names by the locale collation, then
  bytes. A fresh index and a synced one draw the same picture. Upstream takes
  the store's row order, so where two equally long ways reach one step, the one
  drawn can differ from upstream's.
- Each screen a navigation is filed on gets a site of its own, with that
  screen's chain of conditions. Upstream reuses one site object for every
  screen and rewrites its `when` for each, so its second screen repeats the
  chain's conditions and every link holding the site shows the last value.
- A component wrapped where it is declared (`memo(Capture)`,
  `styled(Button)`) has no edge from the wrapper to what it wraps, upstream's
  `wrapped` edge, and the reference to the wrapped component is made by the
  file. A navigation inside it is filed on the screen of the file that holds
  it, with an empty `via`, rather than through the wrapper.
- The Symbol view's conditions keep upstream's 40 ms budget: past the focal
  file, how many files are read depends on the machine, so a row from another
  file can come back without its condition.
- Routes come from the framework resolvers of this port's registry that make
  route nodes: the client routers and file-route frameworks (React Router,
  TanStack Router, Vue Router, Angular Router, Next.js, Nuxt, SvelteKit,
  Astro and Expo Router) and the server frameworks (Laravel, Drupal,
  Express, NestJS, Django, Flask, FastAPI, Rails, Spring, Play, Go, GoFrame,
  Rust's axum and actix, ASP.NET and Vapor).
- The Rust index synthesizes the edges of the passes the
  [architecture's synthesis section](architecture.md#synthesis) lists, the
  cross-tier HTTP client, queue job, event bus and test request edges among
  them, so a flow shows a dashed `via …` hop for those and for no other
  upstream synthesis pass
  ([KNOWN_DIFFS](upstream-sync/KNOWN_DIFFS.md#deferred-colby-resolution-behavior-task-20-codegraph-resolve)
  lists the ported ones). A Go interface's implicit implementations are
  in its type hierarchy, marked synthesized (`go-implements`).
- An unresolved import is recorded by the binding it names, not by the module
  specifier, and the File view lists it that way.
- The bundle is embedded in the binary, so upstream's `CODEGRAPH_VIEWER_PATH`
  has no counterpart.
- An exported Map writes each box's label, the text the canvas draws and sized
  the box for. Upstream's export writes the id, cut to fit, so a Maven package
  reads `src/main/java/com/ac…` in the image.
- A Steps picture of 24 boxes or fewer is drawn whole: beside the open key when
  it fits there at a zoom of 0.2 or more, else on the whole canvas, at a zoom of
  no less than 0.2 either way; one too wide even for that is centred. Upstream
  (`e8ddc4e2`, the same code at its main `461a5d87`) keeps the key's room at a
  0.4 floor, so a wide row of effects runs off the canvas's right edge, and a
  canvas narrower than the key's room pushes the whole picture off it.
- The Steps and Screens keys never run past their canvas: the rows scroll under
  the key's button. Upstream's keys have no height limit, so on a canvas shorter
  than the Steps key the top of the key is cut off, with the button that closes
  it.
- The Symbol view's two rails write each condition in full, wrapping a long one:
  a Calls row grows a line per condition, and a Called by condition wraps inside
  its card. Upstream cuts a condition at the row's right edge, with no ellipsis,
  and its Calls row prints the whole file path before it.

## Look and themes

The viewer is drawn in direction D of [`design/viewer-d.md`](design/viewer-d.md):
an icon nav rail on the left, a command bar with the `⌘K` / `Ctrl+K` search
palette, the trail ribbon under it while a trail exists, and each view as
rounded islands 8 px apart. Below 1024 px wide the rail narrows, the Symbol
view's Called by rail becomes a drawer behind a header button, and the Steps and
Screens keys start closed whatever the reader last chose; below 600 px a bottom
tab bar (Start, Map, Symbol, Flow, More) replaces the rail, the Symbol view
shows one pane at a time (Code, Called by, Calls), and tapping a call in the
code opens a sheet with that line's calls. At any width an open Steps or Screens
key stays inside its canvas: its rows scroll under the button that closes it.
Those two keys store their open or closed state only when the reader opens or
closes one, so a narrow screen's closed start never changes the desktop's.

There are two themes with the same token names: **Nebula** (dark, §3.1 of the
spec) and **Daylight** (light, §3.6). The viewer follows the operating system's
`prefers-color-scheme` until the reader picks one: the theme control — System,
Dark, Light — sits in the nav rail's foot on desktop and tablet and in the More
sheet on the phone. The choice is stored in the browser's `localStorage` under
`codegraph-ui.theme` and applied as `data-theme` on the page; System removes
it. Copy image and Download SVG on the Flow and Map views paint the theme on
screen. `ui/tests/theme-contrast.test.ts` reads both token sets out of
`ui/src/lib/theme.css` and fails if any pair the spec declares falls below
4.5:1.

Interface text is set in Inter, code and names in JetBrains Mono, both bundled
(OFL) so the viewer never fetches a font; ligatures are off, so `->` reads as
written. The icons are Lucide (ISC) geometry, copied into
`ui/src/lib/icons.ts` with its notice.

### Direction D as built — 偏离（画板 vs 落地，供 owner 复核）

| item                     | board                                          | landed                                                                                                                                                                                     |
| ------------------------ | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| project switch           | name, chevron and a `main · <sha>` branch pill | the name alone: one project per viewer, and the API carries no git branch                                                                                                                  |
| rail foot                | keyboard and settings                          | the theme switch above them; keyboard opens the shortcut sheet, settings a panel of what this viewer reads (port, read-only and watching are command-line choices)                         |
| Saved trails destination | a trails screen                                | opens Entry points, which lists saved trails first                                                                                                                                         |
| Start, entry card        | "Entry functions" with Flow buttons            | "Where it starts" — routes when the project is routed, else the files that run something at their top level — opening the row; a flow starts from Entry points                             |
| Flow                     | 190 × 168 step cards and a step-detail panel   | upstream's cards opened at each call window, restyled; Every hop table below; its Runs when column reads each hop's condition, "—" for a call that always runs                             |
| Map toolbar              | zoom readout and an Include tests switch       | the zoom controls; Include tests lives in the inspector's View section; Open crate is Open first file                                                                                      |
| Map key                  | the one-line legend                            | the same, opening to upstream's full key on request (upstream opened the full key by default)                                                                                              |
| File, This file          | edges in and out, prod and test files          | how many files depend on it and it on them, its symbols, its size — no edge totals in the payload                                                                                          |
| hierarchy                | a supertrait outside the index drawn at 50 %   | not drawn: the payload carries no outside-index supertypes                                                                                                                                 |
| code lines               | a long line ends in `…`                        | a long line scrolls sideways, as upstream's did                                                                                                                                            |
| trail hops               | qualified names                                | the symbol's name, as upstream labels a hop                                                                                                                                                |
| SVG export               | — (upstream: always light)                     | the theme on screen                                                                                                                                                                        |
| ligatures                | the boards show `->` as an arrow               | off (§13 left it open; source fidelity decides)                                                                                                                                            |
| tablet Symbol            | blast radius as two stat tiles under the code  | the four tiles stay at the foot of the Called by drawer                                                                                                                                    |
| Screens (D-08)           | cyan focus path, the rest dimmed, amber band   | upstream's canvas in D's colours: thin route frames, dashed origins on top, grey ways lit when a screen is selected; the key opens over the lower left, and below 1024 px it starts closed |
| Steps (D-09)             | amber fork chip, green/red arms, step list     | upstream's canvas in D's colours: a framed decision box with neutral yes/no pills, dashed boxes for calls leaving the index; the key opens on the left, and below 1024 px it starts closed |

Layout constants the components measure against moved with the design; the
suites pin them by name: hierarchy rows 24 → 26 and indent 22 → 28; file
outline rows at a 30 px pitch; map nodes 40 → 52 high, layers 74 → 44 and boxes
34 → 28 apart; callee rows 34 → 44 high with a 52 px pitch, except that a row
that carries conditions grows a line for each, and the next row keeps 8 px under
it (D-02 draws every row 44 high). Character advances
were re-measured in Chrome against the bundled fonts: the map label (12 px
JetBrains Mono 500) 7.81 → 7.2, the map meta line (11 px Inter) 5.9 → 5.5, the
flow link label (11 px mono) 6.65 → 6.6, the Screens pill (10.5 px mono) 6.3,
unchanged.

## Developing the frontend

The frontend lives in `ui/` with its own pinned dependencies and a committed
`ui/package-lock.json`; `npm ci` is the only install path. `npm run build` (or
`make ui`, which runs `npm ci` first) writes
the production bundle into `crates/codegraph-ui/viewer/`, which the crate's
`build.rs` embeds at compile time, so `cargo build` and `cargo install --git`
never need Node. The bundle is committed, and `make ui-check` (the CI `UI` job)
runs `npm ci`, `svelte-check`, the vitest suites and a fresh build, then fails if
the committed bundle is not byte-for-byte that build. `make pre-ci` includes it.

The Rust side is tested against indexed fixture projects in
`crates/codegraph-ui/tests/` (each file a port of the upstream suite it is named
after) and the CLI in `crates/codegraph-cli/tests/cli_ui.rs`; the frontend's
own suites are in `ui/tests/`.
