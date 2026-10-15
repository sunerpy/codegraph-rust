# SQLite and FTS5 data model

This is the AS-BUILT storage contract owned by `codegraph-store`.

- Current schema version: **9**
- Base DDL: `crates/codegraph-store/src/schema.rs`
- Ordered migrations: `crates/codegraph-store/src/migrations.rs`
- Connection and lease policy: `crates/codegraph-store/src/connection.rs`
- Canonical schema oracle: `reference/golden/*/schema.sql` and
  `crates/codegraph-store/tests/schema_parity.rs`

`scripts/docs-check.py` derives the version from source and checks this document,
so version prose cannot silently drift.

## Initialization and migration

A new database is configured before its first page is written, then receives the
complete base schema. The base DDL inserts schema row `1`; initialization also
records the current version with description `Initial schema includes all
migrations`. Existing databases run every missing migration in ascending order,
each in its own transaction.

Migrations that add a column first inspect `PRAGMA table_info`. This makes replay
safe when a newer base schema already contains the column while still failing
loudly if the table itself is absent.

| Version | Change                                                                       |
| ------: | ---------------------------------------------------------------------------- |
|       1 | Core nodes, edges, files, unresolved references, FTS5, triggers, and indexes |
|       2 | `project_metadata`; unresolved-ref file/language context; edge provenance    |
|       3 | `idx_nodes_lower_name` expression index                                      |
|       4 | Drop redundant single-column edge source/target indexes                      |
|       5 | `nodes.return_type` for receiver-type inference                              |
|       6 | `unresolved_refs.reference_subkind` for structural/framework evidence        |
|       7 | Deduplicate edges and add their unique semantic identity index               |
|       8 | `files.generated` plus the partial generated-file index                      |
|       9 | `synthesis_inputs` plus the partial synthesized-edge site index              |

Schema version and extraction version are independent. A DDL change needs a
schema migration; a change in graph meaning may need an extraction-version move
even when DDL is unchanged.

## Application tables

### `schema_versions`

| Column        | Type                | Contract               |
| ------------- | ------------------- | ---------------------- |
| `version`     | INTEGER primary key | applied schema version |
| `applied_at`  | INTEGER not null    | epoch milliseconds     |
| `description` | TEXT nullable       | migration description  |

### `nodes`

| Column                                                | Type              | Contract                                     |
| ----------------------------------------------------- | ----------------- | -------------------------------------------- |
| `id`                                                  | TEXT primary key  | stable node ID                               |
| `kind`                                                | TEXT not null     | serialized `NodeKind`                        |
| `name`                                                | TEXT not null     | display/local name                           |
| `qualified_name`                                      | TEXT not null     | language/container-qualified name            |
| `file_path`                                           | TEXT not null     | project-relative `/` path                    |
| `language`                                            | TEXT not null     | serialized `Language`                        |
| `start_line`, `end_line`                              | INTEGER not null  | 1-based source lines                         |
| `start_column`, `end_column`                          | INTEGER not null  | extractor columns                            |
| `docstring`                                           | TEXT nullable     | documentation text                           |
| `signature`                                           | TEXT nullable     | callable/declaration signature               |
| `visibility`                                          | TEXT nullable     | language visibility                          |
| `is_exported`, `is_async`, `is_static`, `is_abstract` | INTEGER default 0 | boolean flags                                |
| `decorators`, `type_parameters`                       | TEXT nullable     | JSON arrays                                  |
| `return_type`                                         | TEXT nullable     | normalized result type (migration 5)         |
| `updated_at`                                          | INTEGER not null  | epoch milliseconds; canonicalizer removes it |

The ordinary symbol ID formula is documented in
[`equivalence.md`](equivalence.md). Framework-created nodes with an explicitly
documented compatibility ID are the exception, not a second general formula.

### `edges`

| Column             | Type                              | Contract                                       |
| ------------------ | --------------------------------- | ---------------------------------------------- |
| `id`               | INTEGER primary key autoincrement | row identity; not semantic                     |
| `source`, `target` | TEXT not null                     | foreign keys to `nodes(id)`, cascade on delete |
| `kind`             | TEXT not null                     | serialized `EdgeKind`                          |
| `metadata`         | TEXT nullable                     | JSON object                                    |
| `line`, `col`      | INTEGER nullable                  | source location when known                     |
| `provenance`       | TEXT nullable default NULL        | resolution/synthesis provenance                |

A synthesized edge, one the resolver infers rather than reads from a call site
(dynamic dispatch, callbacks, cross-tier channels), carries
`provenance = 'heuristic'` and a `metadata` object with `synthesizedBy` (the pass
that made it) and, when the wiring happens elsewhere, `registeredAt`
(`path:line` of the registration). Consumers such as the type hierarchy and the
named-symbol flow recognize synthesized edges by that provenance value.

A value reference is a `references` edge the extractor emits from a symbol to a
constant or variable of the same file that the symbol reads (upstream #895,
#897), so `impact` on a shared value reaches its readers. It carries `metadata`
`{"valueRef": true}` and no location. Only a constant or variable declared at
file, class, module, struct or enum scope with a distinctive name (three or more
characters, one of them an uppercase letter or `_`) is a target, and none whose
name the file binds again in an inner scope. Generated files have none. The
languages are TypeScript, JavaScript, TSX, ArkTS, Go, Python, PHP, Scala, Rust,
Ruby, C, Pascal (whose targets are its `const`s only), Java, C#, Kotlin, Swift
and Dart, and the scripts of Vue, Svelte and Astro components through the
extractor they delegate to.

An edge's semantic identity is:

```text
(source, target, kind, IFNULL(line, -1), IFNULL(col, -1))
```

`idx_edges_identity` is unique on that tuple. `INSERT OR IGNORE` therefore
removes repeat-pass duplicates, including coordinate-less edges.

### `files`

| Column                      | Type                       | Contract                                        |
| --------------------------- | -------------------------- | ----------------------------------------------- |
| `path`                      | TEXT primary key           | project-relative path                           |
| `content_hash`              | TEXT not null              | full SHA-256 content digest                     |
| `language`                  | TEXT not null              | serialized language ID                          |
| `size`                      | INTEGER not null           | bytes                                           |
| `modified_at`, `indexed_at` | INTEGER not null           | epoch milliseconds                              |
| `node_count`                | INTEGER default 0          | extracted nodes for the file                    |
| `errors`                    | TEXT nullable              | JSON array of extraction errors                 |
| `generated`                 | INTEGER not null default 0 | path/header generated-file signal (migration 8) |

Migration 8 deliberately does not read source to backfill `generated`; older rows
remain zero until re-extraction. Readers combine the stored flag with compatible
path classification where required.

### `unresolved_refs`

| Column                             | Type                              | Contract                                      |
| ---------------------------------- | --------------------------------- | --------------------------------------------- |
| `id`                               | INTEGER primary key autoincrement | row identity                                  |
| `from_node_id`                     | TEXT not null                     | foreign key to source node, cascade on delete |
| `reference_name`, `reference_kind` | TEXT not null                     | unresolved target evidence                    |
| `line`, `col`                      | INTEGER not null                  | reference location                            |
| `candidates`                       | TEXT nullable                     | JSON array                                    |
| `file_path`                        | TEXT not null default `''`        | source file context (migration 2)             |
| `language`                         | TEXT not null default `unknown`   | source language (migration 2)                 |
| `reference_subkind`                | TEXT nullable                     | structural/framework label (migration 6)      |

### `synthesis_inputs`

| Column      | Type             | Contract                                                      |
| ----------- | ---------------- | ------------------------------------------------------------- |
| `file_path` | TEXT primary key | a file that fed a synthesis pass; cascades with `files(path)` |

A sync reads it to tell whether a deleted or edited file changes synthesized
edges, so it can recompute them without re-reading every file (migration 9).

### `project_metadata`

| Column       | Type             | Contract           |
| ------------ | ---------------- | ------------------ |
| `key`        | TEXT primary key | metadata name      |
| `value`      | TEXT not null    | serialized value   |
| `updated_at` | INTEGER not null | epoch milliseconds |

Module-surface rows record what other files' resolution reads from a
JavaScript-family module's own text, which no node records: its re-exports and
export aliases, the CommonJS default, whether it has an `export default`
statement, the names its export lists give, whether it is a sealed module, and a
Vue or Svelte component's `<script>` blocks. `js_module_surface:<path>` holds the
JSON `{"digest", "sources", "targets"}`: the sha256 of the canonical encoding of
those facts, the specifiers the module forwards, and every project file they
resolved to when the row was written, in each language an import following them
can be written in (TypeScript, TSX, JavaScript, JSX, Vue, Svelte, Astro, ArkTS
order extensions differently), sorted. A file whose surface is the default one
(an ES module none of whose text another file reads) has no row.
`js_module_surface_version` records the row format (2).

A full index and a migration write every row and the marker once the file rows
are stored, before resolution. A sync withdraws the marker before its first
mutation and writes the rows it changed, then the marker, only after resolution
is persisted. A sync that finds the marker missing, or another format,
reconciles every JavaScript-family file before writing it. These rows are no
canonical surface; the sync matrix checks that a sync leaves them as a fresh
index writes them.

Namespace rows record what a qualified C or C++ name reads from a file's own
text: its namespace aliases (`namespace py = pybind11;`, in `.h`, `.hh`,
`.hpp`, `.hxx`, `.inl`, `.c`, `.cc`, `.cpp`, `.cxx` files), the namespace
macros a header defines (`.h`, `.hh`, `.hpp`, `.hxx`, `.h++`, `.inl`, `.ipp`,
`.tcc`: openers with their body as written, single-parameter openers, closers
with the scopes they close, identifier macro aliases), and, for a C or C++ file,
the lines that use an opener or closer the project defines (line, macro, its
argument). `cpp_namespace_surface:<path>` holds the JSON `{"facts": {"aliases",
"macros"}, "usage"}`; a file with none of these has no row. A full index and a
migration write them with the module-surface rows; a sync writes the rows it
changed last, with the module-surface rows. When an alias or a macro is defined
in several files, the first by path wins, and within a file its first
definition.

`cpp_type_surface_version` (2) records that no sync left a C or C++ reference
unrefreshed whose resolution reads another file's text (a same-stem header's
declarations, the aliases and class heads an alias lookup walks, the namespace
aliases and macros a qualified name reads) and that the namespace rows are
current. A full index and a migration write it with the module-surface marker;
a sync withdraws it before its first mutation and writes it back last, after
resolution is persisted, and a sync that finds it missing or at another version
re-resolves every C and C++ file and rebuilds the namespace rows before writing
it. Neither is a canonical surface.

Head rows record what Java resolution reads of a Java file's type declarations:
for each class, interface and enum, the names the scope walk of a bare call
reads from the lines of its head and the names its ancestor walk reads, by the
same readers. `java_type_heads:<path>` holds the JSON `{"entries": [{"name",
"qualified_name", "supertypes", "ancestors"}]}`, the entries in the order of the
declarations in the file (the walk reads same-named declarations in that order,
across files by path); a declaration whose head names nothing has no entry, and
a file with no entry has no row. A full index and a migration write them with
the namespace rows; a sync writes the rows it changed last, with the other rows.
`java_type_heads_version` (1) records that no sync left a Java reference
unrefreshed whose resolution reads another file's heads or the project's
packages, and that the head rows are current; it is written, withdrawn and
checked as `cpp_type_surface_version` is, and a sync that finds it missing or at
another version re-resolves every Java file and rebuilds the head rows. Neither
is a canonical surface.

Kotlin surface rows record what Kotlin resolution reads of a Kotlin file (`.kt`,
`.kts`) by the readers it uses: `kotlin_surface:<path>` holds the JSON
`{"heads", "function_types", "extensions", "lambdas", "aliases", "starts",
"lambda_calls"}`: each class, struct, interface and enum head's name, qualified
name and supertypes, the receivers its function types and extension functions
name, each function's name and last parameter type as written, each typealias's
name and right side (each list in file order), and, sorted, the names a
hierarchy walk in the file can start from (type names, member owners, brace
heads, a Gradle script's `Project` or `Settings`) and the calls its lambdas are
passed to. A file with none of these has no row. They are written, withdrawn and
checked like the head rows under `kotlin_surface_version` (1); a sync that finds
it missing or at another version re-resolves every Kotlin file and rebuilds the
rows. Neither is a canonical surface.

Scala surface rows record what Scala resolution reads of a Scala file by the
readers it uses: `scala_surface:<path>` holds the JSON `{"heads",
"package_objects", "starts", "extension_functions", "block_locals",
"object_package", "holder"}`: each type head's name and the supertypes that
declaration names (in node order), each `package object X extends …`'s name and
supertypes (in text order, `.scala` files only), sorted, the names the file's
own walks start from (its type names, every type a `new … with …` names, the
objects it imports whole, its package segments), the functions an `extension`
line marks and the `val`s a block of a class body scopes (by node id and name),
the package its package object opens and the last segment of its last package
clause. A file with none of these has no row. They are written, withdrawn and
checked like the head rows under `scala_surface_version` (1); a sync that finds
it missing or at another version re-resolves every Scala file and rebuilds the
rows. Neither is a canonical surface.

C# surface rows record what C# resolution reads of a C# file by the readers it
uses: `csharp_surface:<path>` holds the JSON `{"globals", "global_statics",
"heads"}`: sorted, the namespaces its `global using`s import and the types its
`global using static`s name, and each class, interface, struct and enum head's
name, qualified name and the bases each of the two readers reads from it
(`bases`, seven lines; `supers`, nine lines with `<…>` and `(…)` dropped; in
node order; a head listing no base to either has no entry). A file with none of
these has no row. They are written, withdrawn and checked like the head rows
under `csharp_surface_version` (2); a sync that finds it missing or at another
version re-resolves every C# file and rebuilds the rows. Neither is a canonical
surface.

Member surface rows record what a C#, Java, Kotlin or Swift call through a declared
member reads of a file's classes: `member_surface:<path>` holds the JSON
`{"classes": [{"name", "qualified_name", "supers", "digest"}]}`, one entry per
class, struct, interface and enum in node order, with the supertypes its head
names and a digest of its member-level lines, its head (nine lines, cut at the
first `{`) and the type parameters it declares. A file with no such declaration
has no row. They are written, withdrawn and checked like the head rows under
`member_surface_version` (2); a sync that finds it missing or at another
version re-resolves every C#, Java, Kotlin and Swift file and rebuilds the
rows.
Neither is a canonical surface.

Ruby head rows record what a bare Ruby call reads of a file's classes and
modules: `ruby_heads:<path>` holds the JSON `{"entries": [{"qualified_name",
"kind", "superclass", "mixins"}]}`, one entry per class and module in file
order, with the superclass its first line writes (`::` kept when it roots the
name) and every `include`, `extend` and `prepend` constant a line of its body
writes, as written. A file with no class or module has no row. They are
written, withdrawn and checked like the head rows under `ruby_heads_version`
(1); a sync that finds it missing or at another version re-resolves every Ruby
file and rebuilds the rows. Neither is a canonical surface.

CFML component rows record what a bare call in a component reads of a `.cfc`
file: `cfml_heads:<path>` holds the JSON `{"components", "extends"}`, the
lowercased names of the classes the file declares and the `extends` path its
text writes before its first function, as written. A `.cfc` file with neither
has no row. They are written, withdrawn and checked like the head rows under
`cfml_heads_version` (1); a sync that finds it missing or at another version
re-resolves every CFML file and rebuilds the rows. Neither is a canonical
surface.

Swift head rows record what Swift type resolution reads of a file's type
declarations: `swift_heads:<path>` holds the JSON `{"entries":
[{"qualified_name", "kind", "extension", "supertypes", "inherits", "written",
"reducer"}]}`, one entry per class, struct, enum, protocol and type alias,
sorted, with whether the declaration is an `extension`, the last segments of
the supertypes its head names by the two readers resolution uses, the nested
path an extension writes (`API::PackageController` for `extension
API.PackageController`) and whether an enum is a TCA `@Reducer`. A file with no
type declaration has no row. They are written, withdrawn and checked like the
head rows under `swift_heads_version` (2); a sync that finds it
missing or at another version re-resolves every Swift file and rebuilds the
rows. Neither is a canonical surface.

Objective-C head rows record what a message reached by name reads of a file's
classes and properties: `objc_heads:<path>` holds the JSON `{"entries":
[{"kind", "name", "written"}]}`, one entry per class (`@interface`,
`@implementation` and category alike) and property, sorted, with the
superclass a class's first line names after `@interface Name :` and the class
a property's `@property` line declares it as, or `null`. A file with neither
has no row. They are written, withdrawn and checked like the head rows under
`objc_heads_version` (1); a sync that finds it missing or at another version
re-resolves every Objective-C file and rebuilds the rows. Neither is a
canonical surface.

Go head rows record what a Go call reads of a file's type declarations:
`go_heads:<path>` holds the JSON `{"entries": [{"name", "embeds", "mentions",
"digest", "imports"}]}`, one entry per struct, interface and type alias (a defined type `type A B` or an
alias `type A = B`), sorted, with the names of the types its declaration embeds, every name its
declaration's text mentions, a 64-bit FNV-1a digest of that text (comments
stripped), and the import path each package qualifier the declaration writes
binds in its file (`dep=example.com/app/store1`). A file
with no type declaration has no row. They are written, withdrawn and checked
like the head rows under `go_heads_version` (1); a sync that finds it missing
or at another version re-resolves every Go file and rebuilds the rows. Neither
is a canonical surface.

PHP head rows record what a PHP call reads of a file's types: `php_heads:<path>`
holds the JSON `{"entries": [{"qualified_name", "kind", "supers"}]}`, one entry
per class, trait, interface and enum, sorted, with the qualified names
(`App\\Models::Entity`) its `extends` clause and its body's trait `use`s name,
each read through the file's `namespace` and `use` imports. A file with no type
has no row. They are written, withdrawn and checked like the head rows under
`php_heads_version` (1); a sync that finds it missing or at another version
re-resolves every PHP file and rebuilds the rows. Neither is a canonical
surface.

Lua module rows record what a Lua call in another file reads of a Lua file:
`lua_module:<path>` holds the JSON `{"locals", "reads", "globals", "surface"}`:
every `local` declaration in source order as `[name, kind, line]`, the modules
its aliases `require` (read from each variable's initializer, as resolution
reads it, and from every `local … = …` line), the last member of every dotted
chain written after an `=` (each shorter chain's too, read in the code and with
its comments removed), and a digest of its functions and its variables with
their locality, both in source order, and of the first export of each name with
the declaration of the local its right side starts from. Every Lua file has a
row. They are written, withdrawn and checked like the head rows under
`lua_modules_version` (1); a sync that finds it missing or at another version,
or a row it cannot read, withdraws the marker before changing anything,
re-resolves every Lua file and rebuilds the rows, even with nothing changed.
Neither is a canonical surface.

The case-folded lane stores no rows, only its marker:
`case_folded_names_version` (1) is written by a full index, a migration and
every sync whose resolution is persisted, and withdrawn with the others before
a sync's first change. A sync that finds it missing or at another version
re-resolves every PHP, Pascal and CFML file. It is not a canonical surface.

The R base-binding lane stores no rows, only its marker:
`r_base_bindings_version` (1) is written by a full index, a migration and every
sync whose resolution is persisted, and withdrawn with the others before a
sync's first change. A sync that finds it missing or at another version
re-resolves every R file. It is not a canonical surface.

The Rust receiver lane stores no rows, only its marker: `rust_receivers_version`
(1) is written by a full index, a migration and every sync whose resolution is
persisted, and withdrawn with the others before a sync's first change. A sync
that finds it missing or at another version re-resolves every Rust file. It is
not a canonical surface.

## FTS5

`nodes_fts` is an external-content FTS5 table over `nodes`:

```sql
CREATE VIRTUAL TABLE nodes_fts USING fts5(
  id,
  name,
  qualified_name,
  docstring,
  signature,
  content='nodes',
  content_rowid='rowid'
);
```

`nodes_ai`, `nodes_ad`, and `nodes_au` maintain FTS rows after node insert,
delete, and update. No tokenizer is specified, so the bundled SQLite FTS5 default
applies. The bundled build must support FTS5; schema creation fails instead of
silently switching to a weaker search backend.

FTS5 shadow tables and SQLite-owned `sqlite_sequence`/`sqlite_stat1` can appear in
`.schema`. The canonicalizer accounts for the expected internal objects while
preserving application DDL.

## Application indexes

The base schema creates:

- node indexes on kind, name, qualified name, file path, language,
  `(file_path, start_line)`, and `lower(name)`;
- edge indexes on kind, `(source, kind)`, `(target, kind)`, provenance, the
  unique semantic identity tuple, and the partial `idx_edges_synthesis_site`
  over synthesized edges' `metadata.registeredAt` (only rows whose metadata
  names `synthesizedBy`; `json_valid` guards keep malformed metadata out);
- file indexes on language, modified time, and the partial `generated = 1` set;
- unresolved-reference indexes on source node, name, file path, and
  `(from_node_id, reference_name)`.

The former `idx_edges_source` and `idx_edges_target` are intentionally absent:
the composite indexes cover their left-prefix lookups and migration 4 removes
them from old databases.

## Connection policy

Normal write-capable connections are configured in this order:

```sql
busy_timeout = 5000 ms
foreign_keys = ON
journal_mode = WAL
journal_size_limit = CODEGRAPH_WAL_VALVE_MB (default 256 MiB)
synchronous = NORMAL
cache_size = -64000
temp_store = MEMORY
mmap_size = 268435456
```

A rebuild validates its exclusive `IndexLease` between file-touching PRAGMAs.
Bulk indexing temporarily uses `synchronous=OFF`, a larger cache/mmap window, and
normally disables WAL autocheckpoint; a fallible finalizer restores defaults,
compacts, and publishes the completed namespace. Early failure leaves the
building namespace unreadable rather than publishing a partial database.

Readers and writers are state-gated. The permanent lock file is a capability tied
to the resolved database parent; a path string or stale PID alone is not write
authority.

## Canonical schema proof

Run the store tests and full oracle rather than comparing prose:

```bash
cargo test --locked -p codegraph-store schema_parity
cargo test --locked -p codegraph-bench --test equivalence
```

The oracle normalizes schema syntax consistently, but it does not make SQLite
file bytes reproducible. Database page counters, WAL state, and row IDs are not
canonical graph output; committed `schema.sql` and canonical JSON are.
