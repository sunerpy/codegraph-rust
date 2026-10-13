# CodeGraph architecture

This page describes the current Rust implementation. It covers ownership and
data flow; detailed CLI, MCP, storage, and golden contracts live in their owning
references.

## Workspace graph

The root `Cargo.toml` workspace list and each crate manifest are authoritative.
The following diagram shows normal (shipped) intra-workspace dependencies; test
and benchmark-only development dependencies are omitted.

```mermaid
graph TD
    core[codegraph-core]
    extract[codegraph-extract]
    store[codegraph-store]
    resolve[codegraph-resolve]
    graph[codegraph-graph]
    mcp[codegraph-mcp]
    watch[codegraph-watch]
    daemon[codegraph-daemon]
    ui[codegraph-ui]
    cli[codegraph-rs / codegraph binary]
    bench[codegraph-bench]

    extract --> core
    store --> core
    resolve --> core
    resolve --> extract
    resolve --> store
    graph --> core
    graph --> store
    mcp --> core
    mcp --> graph
    mcp --> resolve
    mcp --> store
    watch --> core
    watch --> extract
    watch --> resolve
    watch --> store
    daemon --> core
    daemon --> mcp
    daemon --> watch
    ui --> core
    ui --> extract
    ui --> store
    ui --> resolve
    ui --> graph
    ui --> mcp
    ui --> watch
    cli --> core
    cli --> extract
    cli --> store
    cli --> resolve
    cli --> graph
    cli --> mcp
    cli --> watch
    cli --> daemon
    cli --> ui
    bench --> core
    bench --> extract
```

| Crate               | Responsibility                                                        |
| ------------------- | --------------------------------------------------------------------- |
| `codegraph-core`    | shared types, node IDs, configuration, classification, logging        |
| `codegraph-extract` | source discovery, language/custom extraction, unresolved references   |
| `codegraph-store`   | SQLite/FTS5 schema, migrations, persistence, queries, leases          |
| `codegraph-resolve` | generic import/name matching and framework-aware resolution           |
| `codegraph-graph`   | traversal, impact, query parsing, search ranking                      |
| `codegraph-mcp`     | tool schemas/engine, rmcp stdio and streamable-HTTP transports        |
| `codegraph-watch`   | incremental reconciliation and filesystem watcher                     |
| `codegraph-daemon`  | detached shared process, IPC, and process registries                  |
| `codegraph-ui`      | browser viewer: loopback JSON API, live channel, embedded `ui/` build |
| `codegraph-rs`      | clap CLI, agent installer, orchestration, shipped binary              |
| `codegraph-bench`   | equivalence oracle and benchmark harness; not shipped                 |

The dependency direction keeps extraction independent of persistence and keeps
storage independent of query presentation. The CLI is the product assembly root.

## Indexing data flow

```mermaid
graph LR
    source[Project files] --> scan[scan policy + language detection]
    scan --> extract[tree-sitter / custom extraction]
    extract --> facts[nodes + structural edges + unresolved refs]
    facts --> store[(SQLite + FTS5)]
    store --> resolve[generic + framework resolution]
    resolve --> edges[resolved edges]
    edges --> store
    store --> query[graph traversal + search]
    query --> cli[CLI]
    query --> mcp[MCP]
    watcher[watch events] --> reconcile[incremental reconcile]
    reconcile --> extract
    reconcile --> resolve
```

### Discovery and extraction

`codegraph-extract` applies project configuration, ignore rules, extension
overrides, generated-file signals, and deterministic path ordering before
parallel file parsing. A file then follows one of these paths:

1. a custom/embedded extractor (Vue, Svelte, Astro, Razor, Liquid, MyBatis XML,
   DFM/FMX);
2. a grammar-backed `LanguageSpec` and generic tree-sitter walker; or
3. file-level handling for formats that intentionally expose no language symbols.

The walker emits file/symbol nodes and structural ownership edges immediately,
and so the value references of a file, which need nothing outside it (see
[`data-model.md`](data-model.md#edges)). Calls, imports, type relationships, decorators, and framework evidence that need
global knowledge are stored as unresolved references. Per-file extraction is
fail-closed: an unsafe tree depth or fatal parse condition cannot leave a partial
replacement graph for that file.

File parsing can run in parallel because each input is independent. Persistence
and later resolution restore stable order before output; scheduler completion
order is never a graph contract.

### Storage

`codegraph-store` owns a project-local SQLite database with FTS5. Fresh databases
start at the current schema; older supported databases advance through ordered,
transactional migrations. Foreign keys, WAL behavior, indexes, FTS triggers, and
lease/state gates are configured by the store rather than by callers.

See [`data-model.md`](data-model.md) for the current schema version, tables,
indexes, and migration history.

### Resolution

`ReferenceResolver` reads the complete extracted snapshot (or the bounded
streaming path for very large graphs), then applies deterministic strategies.
Every entry (the full index's batches, the serial pass, a sync's scoped pass)
resolves and persists the wiring references first, imports and `extends`/
`implements` supertypes, and the rest after them, so a call on a receiver
typed as a subclass reaches a method its base declares within one pass. A C
or C++ base is resolved only by C++ name lookup (the scopes around the
deriving class, its file's namespace aliases and `using` lines, a
translation unit's own classes), never by the strategies below:

1. language-aware imports, re-exports, packages, and paths (a CommonJS
   `require('./x')` imports its file, and a call through a `require` binding
   follows `module.exports`, `exports.name = …`, a `require(…).member` and a
   `.default` the way the module exports them; `export * as ns` exports the
   namespace alone, a namespace import's `z.core.fn()` continues into it, and
   `import { default as X }` is the default import; a Python name imported
   from a package follows the package's named and star re-exports to its
   definition, three modules deep);
2. qualified, scoped, receiver/type, lexical, and exact-name matching (a
   receiver written as, or inferred to be, a type the project does not declare
   stops the name guesses, except that a C# call still reaches the one
   project extension method in scope that takes that type as `this` and fits
   its arguments; a class-name receiver also reaches the class
   methods its declared supertypes provide; a receiver-word guess never takes
   a framework-dispatched handler, an unnamed test double or the caller
   itself; in a language that overloads by arity, a call then moves to the
   same-owner overload its argument count fits, or in Swift its argument
   labels), then a
   last-resort fuzzy match that ignores case only in PHP, Pascal and CFML,
   whose identifiers do;
3. detected `FrameworkResolver` implementations; and
4. bounded follow-up passes for relationships that require graph conformance.

The selected target then passes fail-closed correctness gates: language/module
visibility, C-family macro and translation-unit scope, Go local bindings,
external-import locality, and target-kind eligibility. A JS-family import is
from outside the repository only when the importing file's package.json, or
an enclosing one, declares the package (a `workspace:`, `file:` or `link:`
dependency is the repository's own, and a Deno import map counts), or it is a
Node built-in, `@std/…`, a runtime scheme (`node:`, `npm:`, `jsr:`) or a
framework's virtual module (`$app/…`, `astro:…`); an alias the resolver does
not follow (`$lib/…`, `~/…`) stays the project's. Calls in Vue, Svelte and
Astro components follow the JS/TS rules and, like ArkTS, see the JS
built-ins, unless the file imports its own binding of the built-in's name.
A result that lands on
the referencing node itself is kept only when nothing rules it out: a call
written through `super` / `base` / `parent::`, on another object's value, or
from a value's initializer is not a self-call, and a type never inherits from
itself (the other type of its name its written qualifier leads to is taken
instead, or none). A name match from non-test code never reaches a symbol
in a test suite (a test source set, a `tests/` / `spec/` / `e2e/` tree, a
`FooTest.kt` / `test_foo.py` / `foo.test.ts` / `foo_unittest.cc` file): test
support a project ships (`testing/`, `fakes/`, a `*-test` module) stays in
reach, and so does a C or C++ file the reference's translation unit
includes. A type name a declaration around the reference declares as a type
parameter (`class Foo<T>`, `def f[A]`, `<T> T max(`, `template <typename T>`) is
that parameter, never a project type of its name; a Scala def's value
parameter is that parameter, and a bare Scala type name reaches another
type's member only from inside it, a subtype, or a file importing its members
(otherwise the type of that name in scope is taken). A function or type
declared inside a function is reached only from inside it, and a Python name
passed as a value that its function binds (a parameter, an assignment, a
`for` / `as` target) is that local, unless a pytest fixture of its name is in
reach (its own module, or a `conftest.py` above it); a JS/TS name the calling
function binds itself (a parameter, or a `var` / `let` / `const` above the
reference) is that local, never a same-named function elsewhere in its file,
and a name destructured from a call's result (`const { login } = useAuth()`)
is the function the callee returns under that key.
A name match from another file never reaches an ES module's top-level binding
the module does not export (the `export` keyword, an `export { … }` list,
`export default x` and `export default { … }` export it; classic scripts,
CommonJS and `.d.ts` files keep their globals), nor a declaration in a Svelte
instance `<script>` or a Vue `<script setup>` (a `<script module>`, a plain
Vue `<script>` and a type a `<script setup>` exports stay importable). A name
match from another file never reaches into a minified script (`*.min.js`, `*-min.js`, a script
whose text is mostly code on lines of a thousand characters, or a webpack
build), though what an import names still binds and calls inside the script
still resolve; a sync that changes such a script re-resolves the references
naming its symbols, since its verdict can flip with no symbol moving. Pure JS-family alias
bindings may forward to a unique callable; wrappers and ambiguity stop at the
binding. A framework-resolved edge carries its own reachability proof and is not
erased by ordinary source-language visibility (for example Tauri IPC to a
private Rust command).

The registered framework candidates are NestJS, React/Next.js, Vue/Nuxt, Godot,
and Tauri, in a deliberate order. Each activates only when its detection guard
matches, and resolves only references written in its own languages (React
and Vue their scripts', NestJS TypeScript and JavaScript, Tauri the JS-family
`invoke` sites and Rust; Godot stays universal). Godot also extracts project/scene/resource relationships; Tauri bridges
literal JS-family `invoke` calls to uniquely registered Rust commands. Ambiguous
or dynamic targets remain unresolved rather than falling through to a global
same-name guess.

The static/runtime boundaries for supported languages and Godot are documented in
[`languages.md`](languages.md), [`godot.md`](godot.md), and the upstream-difference
ledger in [`upstream-sync/KNOWN_DIFFS.md`](upstream-sync/KNOWN_DIFFS.md).

## Query layer

`codegraph-graph` operates on persisted nodes and edges. It owns callers,
callees, impact, paths, hierarchies, cycle detection, FTS candidates, fallback
matching, filters, and deterministic scoring. Generated/ambient/deprioritized
paths can affect ranking without removing graph facts.

The CLI and MCP share the same query engine for `search`, `explore`, and `node`,
so source selection, staleness handling, and relationship rendering do not drift
between interfaces.

## CLI assembly

The `codegraph-rs` package builds the `codegraph` binary. The CLI owns:

- lifecycle commands (`init`, `sync`, full index, status, recovery);
- query and analysis commands;
- MCP process launch and HTTP/stdio observability commands;
- agent/IDE configuration and Skill installation;
- shell completion and self-update; and
- human/JSON output contracts.

The CLI directly uses `codegraph-watch` for one-shot incremental synchronization
and `codegraph-daemon` for shared MCP service. It keeps logs on stderr so JSON and
MCP stdout remain protocol-clean. The complete command contract is
[`cli.md`](cli.md).

## MCP architecture

`codegraph-mcp` uses rmcp and Tokio for MCP protocol handling over stdio and
streamable HTTP. The source schema defines a known callable tool set and a smaller
default `tools/list` surface; an allowlist can expose any known tool. The current
source-derived counts are validated by `scripts/docs-check.py`, not maintained as
an unchecked architecture constant.

Each tool call resolves its project independently. A pinned server uses its
launch project; an unpinned server can use an explicit `projectPath`, client MCP
roots, or bounded deterministic workspace adoption. Engine caches are reopened
when the database is replaced and do not grant filesystem authority outside a
validated project.

Explicit project routing has a separate, bounded lifecycle layer. The first
access to an existing index goes through a session broker (maximum 32 projects),
which starts or attaches that project's shared daemon, retains a client
connection, and completes one catch-up before query execution. The daemon, not
the MCP handler, owns the watcher and long-lived writer lock. This keeps multiple
child indexes independent without choosing a default or creating state, while
several MCP sessions converge on one watcher owner per project.

Protocol revisions, default methods, tool schemas, project selection, staleness,
and HTTP headers are specified in [`mcp.md`](mcp.md).

## Daemon and watch lifecycle

`serve --mcp` chooses direct mode or the per-project shared daemon. A daemon owns
one project rendezvous, accepts multiple client sessions, starts the watcher, and
exits after its last client has been idle for the configured period. IPC uses Unix
local sockets or Windows named pipes. Project lock/state files stay under the
selected index root; long Unix socket paths may use a deterministic temporary
socket location recorded in the rendezvous metadata.

The watcher maintains per-directory watches outside ignored trees, debounces
bursts, reloads project control files, and reconciles additions, modifications,
deletions, and scope changes through `codegraph-watch`. A burst past 500 paths,
a backend's sign of dropped events, or (on Windows) drift found by an idle
sentinel escalates to one full reconcile, reported as RECOVERING until it
commits. A sync re-resolves the references that name what it changed, a
qualified one (`User.create`) by its last segment as well (at most 500 per
segment); a Python import statement that comes or goes counts the names it
can re-export as changed (what it binds, and for a star import every name the
module offers, as deep as the re-export walk looks), since an importer's name
then moves with no node of it changing; and a changed Python file refreshes the
method values that read through a module global it declares or can write, in
that module and its importers, since the global's type is read from statements
no node records; a changed `conftest.py`, or a module one pulls in, refreshes
the functions under its reach that take a parameter named like one of its
fixtures, since a fixture's return type types that parameter; a
JavaScript-family file whose module surface moved (its re-exports, export
aliases and lists, CommonJS default, sealed-ness or component scripts, kept as
a digest per file in `project_metadata`), and every file that came or went,
sends the direct dependents of it and of the barrels that forward it (by the
targets their rows recorded or what their sources resolve to now) down the
whole-file lane and touches the names it declares, since no node records any
of that text; a sync withdraws the surface marker before its first mutation
and restores it last, so one that fails in between leaves the next sync to
reconcile every JavaScript-family file; a changed C or C++ header (`.h`,
`.hpp`, `.hxx`) sends its same-stem sources (`.c`, `.cc`, `.cpp`, `.cxx`) down
the whole-file lane, since receiver inference reads a member's declared type
from it, and a C or C++ file whose type surface moved (an alias that came,
went or names another type by its recorded declaration, a class-like type or
enum that came or went under an alias's name, a class whose bases changed),
compared with one snapshot of the store taken before the sync first changed a
C or C++ file, sends every unchanged C or C++ file whose text mentions a
touched name down the same lane, the names closed over the aliases whose
declarations mention them and over the classes deriving from a class whose
bases changed, since an alias lookup reads those declarations by name across
the project; a C or C++ file whose namespace row moved (a namespace alias or a
namespace macro definition that came, went or changed, with the names its body
writes, or a changed use of an opener or closer, with the namespaces it opens,
closed over every alias and macro whose body names a seed) sends every
unchanged C or C++ file mentioning a seed down the same lane, together with the
bare names (an operator's or destructor's first identifier) of the
declarations whose effective name, macro prefix plus qualified name, moved in
the changed files that use an opener or closer and in the unchanged files that
use a seed macro, since a qualified name and a call from the global scope read
those namespaces from text; the `cpp_type_surface_version` marker is withdrawn
and restored with the surface marker, and an index without it, or with an
older version, re-resolves every C and C++ file once and rebuilds the namespace
rows (a base that becomes a namesake in another namespace, and a class gaining
or losing its `template <…>` head while no alias's text changes, are not
detected); a Java file whose head row moved (a class, interface or enum whose
head names other types, by the two readers resolution uses and in file order,
came, went or now names others; the rows are kept per file in
`project_metadata`) sends every unchanged file declaring a Java type of a
touched name, or an anonymous class instantiating one, down the whole-file lane,
the names closed over every head that names one, since a bare call's scope walk
and a type name's ancestor walk read those heads by name across the project; a
package the sync makes the project declare, or stop declaring (only Java and
Kotlin files declare one), sends every unchanged Java file with an import from
under it down the same lane, since an import from outside the project owns its
name; the `java_type_heads_version` marker is withdrawn and restored with the
others, and an index without it re-resolves every Java file once and rebuilds
the head rows (a call through a receiver typed as a subtype of a changed head,
in a file declaring no type of a touched name, is not detected); a Kotlin file
whose surface row moved re-resolves, whole, the unchanged Kotlin files whose
walks can start from a name the changed heads reach (closing over the heads
naming a reached name and Android's platform ancestry in one queue) or whose
lambdas are passed to a function whose receiver moved, and touches, for the name
lane, the members of every type that joined or left the project-wide receiver
types and of every owner a Gradle script reads that came or left as a project
class name, since none of those facts is a node; the `kotlin_surface_version`
marker is withdrawn and restored with the others; a Scala file whose surface row
moved re-resolves, whole, the unchanged Scala files whose walks start from a
name its changed heads or package objects reach (closing over every head and
package object naming a reached name), and touches, for the name lane, the
functions and `val`s whose `extension` or block reading flipped (by node id),
every name of the file when the package its package object opens changed, and
its top-level types when the package holding them changed, sending the unchanged
files with references to those names written with a receiver down the
whole-file lane too, however many there are, since none of those facts is a
node; the `scala_surface_version` marker is withdrawn and restored with the
others; a C# file whose `global using`s or `global using static`s changed
re-resolves every unchanged C# file whole, since every file of a project reads
the first and every file the second, and the project is a `.csproj`, which is
no source file (a `.csproj` or `.props` change itself is not yet detected); one
whose surface row's heads moved re-resolves, whole, the unchanged files
declaring a C# type of a name those heads reach (closing over every head
listing a reached name as a base to either reader), since a nested type is
reached from a type deriving from its owner, and a bare member from the types
around it and their bases, through heads read by name; and a C# type
that keeps its node while its owner turns from a type into a namespace of the
same name, or back, has its name touched for the name lane; the
`csharp_surface_version` marker is withdrawn and restored with the others; a
C#, Java or Kotlin file whose member surface row moved (a class's head, type
parameters or member-level lines changed, by a digest, or a class came or went)
re-resolves, whole, the unchanged files declaring a class of a name the changed
classes reach (closing over every head naming a reached name), since a call
through an inherited member reads its type from the classes it inherits by
name; a method body edit changes no row; the `member_surface_version` marker is
withdrawn and restored with the others; a Ruby file whose head row moved (a
class or module came or went, or its superclass or mixins changed) re-resolves,
whole, the unchanged files declaring a class or module the changed qualified
names reach (closing over every declaration writing a constant whose last
segment is a reached name's), since a bare call in a class reads the class's
ancestry from its declarations' source; the `ruby_heads_version` marker is
withdrawn and restored with the others; a `.cfc` file whose component row moved
(it came or went, declares other classes, or extends something else)
re-resolves, whole, the unchanged components whose `extends` names a class the
changed files declare (closing over every component extending a reached name),
since a bare call in a component reads its `extends` chain from the components'
text, and the unchanged CFML files holding a reference written with a receiver
whose last segment names a method a changed component gained or lost (every
method of one that came, went or was renamed) re-resolve whole, however many
there are, since a typed call waits while components of its type's name in
several files declare the method; the `cfml_heads_version` marker is withdrawn
and restored with the others; a Swift file whose head row moved (a type
declaration came or went, became or stopped being an extension, moved to
another qualified name, or names other supertypes) re-resolves, whole, the
unchanged Swift files holding a reference to a name the changed declarations
reach, resolved or parked, or declaring a type of one (closing over every head
naming a reached name), since a type reference reads whether a node is an
extension, a nested type's scope, a qualifier and an extended conformance from
the declarations' text; the `swift_heads_version` marker is withdrawn and
restored with the others; an
import names a file, a folder or a namespace rather than a symbol, so the parked
imports an added file or a changed file's namespace can satisfy (by the file's
name and stem, its folder's, or the namespace's whole name) send their files
down the whole-file re-resolution lane, which rebuilds an unchanged file's
resolved edges and keeps what its extraction produced (`contains` edges and
same-file value references). Each mutation acquires the
store's short-lived `index.lock` writer authority; readers use corroborated state
and shared leases. A separate persistent `writer.pid` file carries an OS kernel
exclusive lock for the lifetime of the ONE process allowed to run watcher/catch-up
services for that project. Its JSON (`pid`, mode, start time) is diagnostic only;
the kernel lock is authority and is released automatically on process exit.

Default daemon mode multiplexes every MCP client onto that one writer. On a cold
start the foreground stdio session answers immediately without its own background
services while the detached daemon takes ownership. Once the daemon's socket is
up, the session retains a passive lease on it, the same client connection an
explicit `projectPath` service holds, so the daemon does not idle-exit while the
session lives. If that lease stops being live (the daemon died, was replaced, or
the index was re-created), the session starts or re-attaches a daemon with
exponential backoff, and never creates an index that is gone. A session that
meets a daemon of an older plain `X.Y.Z` release asks it to drain over the
project-bound control channel and starts one from its own install; a daemon of
any other version keeps the project. Each daemon also exits once its own
executable is replaced or removed. Explicit direct mode
(`CODEGRAPH_NO_DAEMON=1`) may own the writer instead; a second direct process fails
fast rather than alternating sync mutations. An unindexed explicit path remains
state-free: only `codegraph init` creates an index namespace.

Streamable HTTP is different: a server is keyed by bind address and can be pinned
to one project or globally accept explicit per-call projects. Detached HTTP and
foreground stdio processes use global observability registries. Those registries
do not replace project authorization or database leases.

Operational details and environment variables are in [`cli.md`](cli.md) and
[`mcp.md`](mcp.md).

## Determinism boundary

The durable compatibility boundary is the canonical graph, not SQLite file bytes
or process scheduling. Tests lock:

- node ID inputs and file-node IDs;
- canonical nodes, edges, unresolved references, files, and schema;
- incremental/full-index convergence;
- deterministic candidate and output ordering;
- structural MCP compatibility; and
- installer/release asset contracts.

See [`equivalence.md`](equivalence.md). Performance evidence is a separate
measurement discipline described in [`benchmark.md`](benchmark.md).
