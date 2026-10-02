# Browser viewer (`codegraph ui`)

`codegraph ui` opens a local, read-only reader of a project's existing index in
the browser: a symbol with its callers, source and callees side by side, a file's
outline and imports, the call path between two symbols, the repository as a map
of modules, a type's hierarchy, the code nothing reaches, and the places to start
reading an unfamiliar project. It is a Rust port of the upstream
colbymchenry/codegraph `v1.6.1` viewer: the frontend is upstream's `ui/` (Svelte
5 + Vite, MIT, see [`ui/LICENSE`](../ui/LICENSE)) and the JSON API behind it is
the `codegraph-ui` crate.

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

## Developing the frontend

The frontend lives in `ui/` with its own pinned dependencies and a committed
`ui/package-lock.json`; `npm ci` is the only install path. `npm run build` writes
the production bundle into `crates/codegraph-ui/viewer/`, which the crate's
`build.rs` embeds at compile time, so `cargo build` and `cargo install --git`
never need Node. The bundle is committed, and `make ui-check` (the CI `UI` job)
runs `npm ci`, `svelte-check`, the vitest suites and a fresh build, then fails if
the committed bundle is not byte-for-byte that build. `make pre-ci` includes it.

The Rust side is tested against indexed fixture projects in
`crates/codegraph-ui/tests/` (each file a port of the upstream suite it is named
after) and the CLI in `crates/codegraph-cli/tests/cli_ui.rs`; the frontend's
own suites are in `ui/tests/`.
