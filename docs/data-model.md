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
languages are TypeScript, JavaScript, TSX, ArkTS, Go and Python, and the scripts
of Vue, Svelte and Astro components through the extractor they delegate to.

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
