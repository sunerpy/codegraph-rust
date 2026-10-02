# Browser viewer (`codegraph ui`)

`codegraph ui` opens a local, read-only reader of a project's existing index in
the browser: a symbol with its callers, source and callees side by side, a file's
outline and imports, the call path between two symbols, the repository as a map
of modules, a type's hierarchy, the code nothing reaches, and the places to start
reading an unfamiliar project. It is a Rust port of the upstream
colbymchenry/codegraph `v1.6.1` viewer: the frontend is upstream's `ui/` (Svelte
5 + Vite, MIT, see [`ui/LICENSE`](../ui/LICENSE)), restyled to the selected
design ([`design/viewer-d.md`](design/viewer-d.md), direction D) with a dark and
a light theme, and the JSON API behind it is the `codegraph-ui` crate.

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
ports after it; an explicit `--port` never moves. `CODEGRAPH_BROWSER=<command>`
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
| `GET /api/flow?from=&to=` · `?symbols=` · `?hop=…`            | the call path as cards, with where the static graph stops and why                             |
| `GET /api/screens`                                            | screens and the navigation between them                                                       |
| `GET /api/events`                                             | the live channel above                                                                        |
| `GET/POST /api/trails`, `DELETE /api/trails/<id>`             | saved trails, re-resolved; the one write                                                      |

## Differences from upstream

The wire shapes, limits, error codes and refusals follow upstream `v1.6.1`. Where
the Rust graph holds different facts, the viewer shows what this index holds:

- **Steps** (`/api/steps`) is not served yet: its effect and program builders and
  the branch conditions they use (UI families F1–F6 and F11) are a later port.
  The frontend is built without it, so the Steps view says it cannot draw steps.
- **Screens** answers upstream's own "no screen navigation" result: the Rust
  graph has no `navigates` edges until the navigation resolvers are ported.
- Branch conditions on call sites (`when`) are absent from every edge for the
  same reason.
- Routes come from the framework resolvers this port has — React Router and
  Next.js pages, Vue Router, NestJS controllers. Express and Go HTTP routers are
  not among them, so those projects are not routed apps here.
- The Rust index has no synthesized dynamic-dispatch edges, so a flow never
  shows a dashed `via …` hop from synthesis, and a Go interface's implicit
  implementations are not part of its type hierarchy.
- An unresolved import is recorded by the binding it names, not by the module
  specifier, and the File view lists it that way.
- The bundle is embedded in the binary, so upstream's `CODEGRAPH_VIEWER_PATH`
  has no counterpart.

## Look and themes

The viewer is drawn in direction D of [`design/viewer-d.md`](design/viewer-d.md):
an icon nav rail on the left, a command bar with the `⌘K` / `Ctrl+K` search
palette, the trail ribbon under it while a trail exists, and each view as
rounded islands 8 px apart. Below 1024 px wide the rail narrows and the Symbol
view's Called by rail becomes a drawer behind a header button; below 600 px a
bottom tab bar (Start, Map, Symbol, Flow, More) replaces the rail, the Symbol
view shows one pane at a time (Code, Called by, Calls), and tapping a call in
the code opens a sheet with that line's calls.

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

| item                     | board                                          | landed                                                                                                                                                             |
| ------------------------ | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| project switch           | name, chevron and a `main · <sha>` branch pill | the name alone: one project per viewer, and the API carries no git branch                                                                                          |
| rail foot                | keyboard and settings                          | the theme switch above them; keyboard opens the shortcut sheet, settings a panel of what this viewer reads (port, read-only and watching are command-line choices) |
| Saved trails destination | a trails screen                                | opens Entry points, which lists saved trails first                                                                                                                 |
| Start, entry card        | "Entry functions" with Flow buttons            | "Where it starts" — routes when the project is routed, else the files that run something at their top level — opening the row; a flow starts from Entry points     |
| Flow                     | 190 × 168 step cards and a step-detail panel   | upstream's cards opened at each call window, restyled; Every hop table below; its Runs when column reads "—" until conditions are ported                           |
| Map toolbar              | zoom readout and an Include tests switch       | the zoom controls; Include tests lives in the inspector's View section; Open crate is Open first file                                                              |
| Map key                  | the one-line legend                            | the same, opening to upstream's full key on request (upstream opened the full key by default)                                                                      |
| File, This file          | edges in and out, prod and test files          | how many files depend on it and it on them, its symbols, its size — no edge totals in the payload                                                                  |
| hierarchy                | a supertrait outside the index drawn at 50 %   | not drawn: the payload carries no outside-index supertypes                                                                                                         |
| code lines               | a long line ends in `…`                        | a long line scrolls sideways, as upstream's did                                                                                                                    |
| trail hops               | qualified names                                | the symbol's name, as upstream labels a hop                                                                                                                        |
| SVG export               | — (upstream: always light)                     | the theme on screen                                                                                                                                                |
| ligatures                | the boards show `->` as an arrow               | off (§13 left it open; source fidelity decides)                                                                                                                    |
| tablet Symbol            | blast radius as two stat tiles under the code  | the four tiles stay at the foot of the Called by drawer                                                                                                            |

Layout constants the components measure against moved with the design; the
suites pin them by name: hierarchy rows 24 → 26 and indent 22 → 28; file
outline rows at a 30 px pitch; map nodes 40 → 52 high, layers 74 → 44 and boxes
34 → 28 apart; callee rows 34 → 44 high with a 52 px pitch. Character advances
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
