# CodeGraph SQLite/FTS5 Data Model Parity Spec

> AS-BUILT 校核（T27）：本文与已提交代码一致。`CURRENT_SCHEMA_VERSION = 5` 与
> `FRESH_SCHEMA_DESCRIPTION = "Initial schema includes all migrations"`
> （`crates/codegraph-store/src/migrations.rs`）、7 个连接 PRAGMA
> （`connection.rs`，`configureConnection` 步骤的顺序）、`nodes.return_type`
> 列与迁移序列均按本文实现。

This document records the authoritative storage contract for the Rust port. The golden schema and database were captured from the reference implementation run against the deterministic mini fixture at `crates/codegraph-bench/fixtures/mini/`, copied to `/tmp/cg-fixture-mini/` for indexing.

Golden artifacts captured from that run, committed under `reference/golden/`:

- Schema dump: `reference/golden/colby.schema.sql`
- Raw database: `reference/golden/mini/colby.db`
- Nodes table JSON: `reference/golden/mini/colby.nodes.json`
- Database location: `/tmp/cg-fixture-mini/.codegraph/codegraph.db`

## Real `.schema` Dump Contract

`reference/golden/colby.schema.sql` was generated with:

```bash
sqlite3 /tmp/cg-fixture-mini/.codegraph/codegraph.db .schema > reference/golden/colby.schema.sql
```

The dump is post-initialization/post-migration truth. It includes the six application tables (`schema_versions`, `nodes`, `edges`, `files`, `unresolved_refs`, `project_metadata`), `nodes_fts` plus its FTS5 shadow tables, the three FTS triggers, the post-migration indexes, `sqlite_sequence`, and `sqlite_stat1` created by the maintenance/ANALYZE path.

Notably absent in the post-migration state: `idx_edges_source` and `idx_edges_target`. Fresh schema omits them and migration v4 drops them from older databases.

## Tables and Columns

### `schema_versions`

| Column        | Type      |                   Null | Default | Key           | Notes                                 |
| ------------- | --------- | ---------------------: | ------- | ------------- | ------------------------------------- |
| `version`     | `INTEGER` | nullable by SQLite DDL | none    | `PRIMARY KEY` | Schema version number.                |
| `applied_at`  | `INTEGER` |             `NOT NULL` | none    |               | Milliseconds since epoch.             |
| `description` | `TEXT`    |               nullable | none    |               | Migration/initialization description. |

Fresh DB rows from the run: version `1` (`Initial schema`) and version `5` (`Initial schema includes all migrations`).

### `nodes`

| Column            | Type      |                   Null | Default | Key           | Notes                                                                         |
| ----------------- | --------- | ---------------------: | ------- | ------------- | ----------------------------------------------------------------------------- |
| `id`              | `TEXT`    | nullable by SQLite DDL | none    | `PRIMARY KEY` | Stable node identifier.                                                       |
| `kind`            | `TEXT`    |             `NOT NULL` | none    |               | Symbol/file/import kind.                                                      |
| `name`            | `TEXT`    |             `NOT NULL` | none    |               | Display/local name.                                                           |
| `qualified_name`  | `TEXT`    |             `NOT NULL` | none    |               | Qualified symbol name.                                                        |
| `file_path`       | `TEXT`    |             `NOT NULL` | none    |               | Project-relative path.                                                        |
| `language`        | `TEXT`    |             `NOT NULL` | none    |               | Language label.                                                               |
| `start_line`      | `INTEGER` |             `NOT NULL` | none    |               | 1-based start line.                                                           |
| `end_line`        | `INTEGER` |             `NOT NULL` | none    |               | 1-based end line.                                                             |
| `start_column`    | `INTEGER` |             `NOT NULL` | none    |               | Start column as emitted by the extractor.                                     |
| `end_column`      | `INTEGER` |             `NOT NULL` | none    |               | End column as emitted by the extractor.                                       |
| `docstring`       | `TEXT`    |               nullable | none    |               | Optional documentation string.                                                |
| `signature`       | `TEXT`    |               nullable | none    |               | Optional signature.                                                           |
| `visibility`      | `TEXT`    |               nullable | none    |               | Optional visibility (`private`, etc.).                                        |
| `is_exported`     | `INTEGER` |               nullable | `0`     |               | Boolean encoded as integer.                                                   |
| `is_async`        | `INTEGER` |               nullable | `0`     |               | Boolean encoded as integer.                                                   |
| `is_static`       | `INTEGER` |               nullable | `0`     |               | Boolean encoded as integer.                                                   |
| `is_abstract`     | `INTEGER` |               nullable | `0`     |               | Boolean encoded as integer.                                                   |
| `decorators`      | `TEXT`    |               nullable | none    |               | JSON array.                                                                   |
| `type_parameters` | `TEXT`    |               nullable | none    |               | JSON array.                                                                   |
| `return_type`     | `TEXT`    |               nullable | none    |               | Normalized return/result type name; present in fresh schema and migration v5. |
| `updated_at`      | `INTEGER` |             `NOT NULL` | none    |               | Milliseconds since epoch.                                                     |

### `edges`

| Column       | Type      |                   Null | Default | Key                         | Notes                                                                |
| ------------ | --------- | ---------------------: | ------- | --------------------------- | -------------------------------------------------------------------- |
| `id`         | `INTEGER` | nullable by SQLite DDL | none    | `PRIMARY KEY AUTOINCREMENT` | Row id. Creates `sqlite_sequence`.                                   |
| `source`     | `TEXT`    |             `NOT NULL` | none    | FK                          | References `nodes(id)` with `ON DELETE CASCADE`.                     |
| `target`     | `TEXT`    |             `NOT NULL` | none    | FK                          | References `nodes(id)` with `ON DELETE CASCADE`.                     |
| `kind`       | `TEXT`    |             `NOT NULL` | none    |                             | Relationship kind.                                                   |
| `metadata`   | `TEXT`    |               nullable | none    |                             | JSON object.                                                         |
| `line`       | `INTEGER` |               nullable | none    |                             | Source line for relationship if known.                               |
| `col`        | `INTEGER` |               nullable | none    |                             | Source column for relationship if known.                             |
| `provenance` | `TEXT`    |               nullable | `NULL`  |                             | Added by migration v2; provenance tag for heuristic/synthetic edges. |

### `files`

| Column         | Type      |                   Null | Default | Key           | Notes                                |
| -------------- | --------- | ---------------------: | ------- | ------------- | ------------------------------------ |
| `path`         | `TEXT`    | nullable by SQLite DDL | none    | `PRIMARY KEY` | Project-relative source path.        |
| `content_hash` | `TEXT`    |             `NOT NULL` | none    |               | File content hash.                   |
| `language`     | `TEXT`    |             `NOT NULL` | none    |               | Language label.                      |
| `size`         | `INTEGER` |             `NOT NULL` | none    |               | File size in bytes.                  |
| `modified_at`  | `INTEGER` |             `NOT NULL` | none    |               | File modified timestamp.             |
| `indexed_at`   | `INTEGER` |             `NOT NULL` | none    |               | Index timestamp.                     |
| `node_count`   | `INTEGER` |               nullable | `0`     |               | Number of indexed nodes in the file. |
| `errors`       | `TEXT`    |               nullable | none    |               | JSON array.                          |

### `unresolved_refs`

| Column           | Type      |                   Null | Default     | Key                         | Notes                                            |
| ---------------- | --------- | ---------------------: | ----------- | --------------------------- | ------------------------------------------------ |
| `id`             | `INTEGER` | nullable by SQLite DDL | none        | `PRIMARY KEY AUTOINCREMENT` | Row id.                                          |
| `from_node_id`   | `TEXT`    |             `NOT NULL` | none        | FK                          | References `nodes(id)` with `ON DELETE CASCADE`. |
| `reference_name` | `TEXT`    |             `NOT NULL` | none        |                             | Unresolved symbol/reference name.                |
| `reference_kind` | `TEXT`    |             `NOT NULL` | none        |                             | Reference type.                                  |
| `line`           | `INTEGER` |             `NOT NULL` | none        |                             | Reference line.                                  |
| `col`            | `INTEGER` |             `NOT NULL` | none        |                             | Reference column.                                |
| `candidates`     | `TEXT`    |               nullable | none        |                             | JSON array.                                      |
| `file_path`      | `TEXT`    |             `NOT NULL` | `''`        |                             | Added by migration v2; present in fresh schema.  |
| `language`       | `TEXT`    |             `NOT NULL` | `'unknown'` |                             | Added by migration v2; present in fresh schema.  |

### `project_metadata`

| Column       | Type      |                   Null | Default | Key           | Notes                     |
| ------------ | --------- | ---------------------: | ------- | ------------- | ------------------------- |
| `key`        | `TEXT`    | nullable by SQLite DDL | none    | `PRIMARY KEY` | Metadata key.             |
| `value`      | `TEXT`    |             `NOT NULL` | none    |               | Metadata value.           |
| `updated_at` | `INTEGER` |             `NOT NULL` | none    |               | Milliseconds since epoch. |

### SQLite/FTS Internal Tables in `.schema`

- `sqlite_sequence(name,seq)` appears because `edges` and `unresolved_refs` use `AUTOINCREMENT`.
- `sqlite_stat1(tbl,idx,stat)` appears because the maintenance path runs `PRAGMA optimize` after bulk writes.
- FTS5 shadow tables generated by `nodes_fts`: `nodes_fts_data`, `nodes_fts_idx`, `nodes_fts_docsize`, `nodes_fts_config`.

## Indexes (Post-Migration)

Application indexes in the authoritative dump:

```sql
CREATE INDEX idx_nodes_kind ON nodes(kind);
CREATE INDEX idx_nodes_name ON nodes(name);
CREATE INDEX idx_nodes_qualified_name ON nodes(qualified_name);
CREATE INDEX idx_nodes_file_path ON nodes(file_path);
CREATE INDEX idx_nodes_language ON nodes(language);
CREATE INDEX idx_nodes_file_line ON nodes(file_path, start_line);
CREATE INDEX idx_nodes_lower_name ON nodes(lower(name));
CREATE INDEX idx_edges_kind ON edges(kind);
CREATE INDEX idx_edges_source_kind ON edges(source, kind);
CREATE INDEX idx_edges_target_kind ON edges(target, kind);
CREATE INDEX idx_files_language ON files(language);
CREATE INDEX idx_files_modified_at ON files(modified_at);
CREATE INDEX idx_unresolved_from_node ON unresolved_refs(from_node_id);
CREATE INDEX idx_unresolved_name ON unresolved_refs(reference_name);
CREATE INDEX idx_unresolved_file_path ON unresolved_refs(file_path);
CREATE INDEX idx_unresolved_from_name ON unresolved_refs(from_node_id, reference_name);
CREATE INDEX idx_edges_provenance ON edges(provenance);
```

SQLite also creates autoindexes for primary keys: `sqlite_autoindex_nodes_1`, `sqlite_autoindex_files_1`, and `sqlite_autoindex_project_metadata_1`.

## FTS5 Configuration

Authoritative virtual table DDL:

```sql
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    id,
    name,
    qualified_name,
    docstring,
    signature,
    content='nodes',
    content_rowid='rowid'
)
/* nodes_fts(id,name,qualified_name,docstring,signature) */;
```

- FTS engine: SQLite FTS5.
- Indexed columns: `id`, `name`, `qualified_name`, `docstring`, `signature`.
- External content table: `content='nodes'`.
- External content row id: `content_rowid='rowid'`.
- Tokenizer: no explicit tokenizer in the DDL, so SQLite's FTS5 default tokenizer (`unicode61`) applies.

## FTS Trigger Bodies (Verbatim)

```sql
CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
```

```sql
CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
END;
```

```sql
CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
    INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
```

## Migration Sequence

Migration history:

| Version | Description                                                                                                        | Effect                                                                                                                                                                                                                                                  |
| ------: | ------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
|       1 | Initial schema                                                                                                     | Loaded from `schema.sql`; creates core tables, FTS5 table, triggers, indexes, and inserts version 1.                                                                                                                                                    |
|       2 | Add project metadata, provenance tracking, and unresolved ref context                                              | Creates `project_metadata`; adds `unresolved_refs.file_path TEXT NOT NULL DEFAULT ''`, `unresolved_refs.language TEXT NOT NULL DEFAULT 'unknown'`, `edges.provenance TEXT DEFAULT NULL`; creates `idx_unresolved_file_path` and `idx_edges_provenance`. |
|       3 | Add lower(name) expression index for memory-efficient case-insensitive lookups                                     | Creates `idx_nodes_lower_name ON nodes(lower(name))`.                                                                                                                                                                                                   |
|       4 | Drop redundant idx_edges_source / idx_edges_target (covered by source_kind / target_kind composites)               | Drops `idx_edges_source` and `idx_edges_target` if present. They must not exist in the Rust post-migration schema.                                                                                                                                      |
|       5 | Add nodes.return_type — normalized return/result type for receiver-type inference (C++ singletons/factories, #645) | Adds `nodes.return_type TEXT` for older DBs. Fresh schema already contains it.                                                                                                                                                                          |

`CURRENT_SCHEMA_VERSION` is `5`. On fresh initialization, the store loads schema.sql and then records version `5` as `Initial schema includes all migrations` so old migrations are not replayed.

## Connection PRAGMAs

Every connection is configured (the `configureConnection` step) with:

```sql
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 268435456;
```

Maintenance after bulk writes is best-effort:

```sql
PRAGMA optimize;
PRAGMA wal_checkpoint(PASSIVE);
```

`sqlite-adapter.ts` implements `.pragma()` by executing `PRAGMA <key> = <value>` for write pragmas and reading `PRAGMA <key>` for read pragmas using Node's built-in `node:sqlite` `DatabaseSync`.

Live external `sqlite3` query results against the golden reference `.db` under `reference/golden/mini/` after the run:

| Query                      | Result   | Persistence note                                                                        |
| -------------------------- | -------- | --------------------------------------------------------------------------------------- |
| `select sqlite_version();` | `3.52.0` | Version of the external CLI used for capture, not the application's embedded SQLite.    |
| `PRAGMA journal_mode;`     | `wal`    | Persisted in the database.                                                              |
| `PRAGMA synchronous;`      | `2`      | External sqlite3 connection default (`FULL`); the application connection sets `NORMAL`. |
| `PRAGMA foreign_keys;`     | `0`      | Connection-local; the application connection sets `ON`.                                 |
| `PRAGMA busy_timeout;`     | `0`      | Connection-local; the application connection sets `5000`.                               |
| `PRAGMA cache_size;`       | `-2000`  | Connection-local external default; the application connection sets `-64000`.            |
| `PRAGMA temp_store;`       | `0`      | Connection-local external default; the application connection sets `MEMORY`.            |
| `PRAGMA mmap_size;`        | `0`      | Connection-local external default; the application connection sets `268435456`.         |

## Rust `rusqlite` Parity Plan

- Workspace dependency is `rusqlite = { version = "0.31", features = ["bundled"] }`.
- Resolved `libsqlite3-sys` is `0.28.0`.
- The vendored bundled SQLite in `libsqlite3-sys 0.28.0` is SQLite `3.45.0` (`SQLITE_VERSION_NUMBER 3045000`, source id `2024-01-15 17:01:13 1066602b2b1976fe58b5150777cced894af17c803e068f5918390d6915b46e1d`).
- The Rust store must enable the same connection PRAGMAs on every opened connection before schema work or queries.
- The bundled SQLite build must support FTS5; task decisions already require `rusqlite` bundled for FTS5. If future CI disables FTS5, schema creation must fail loudly rather than silently degrading.
- Rust schema application must produce the same post-migration `.schema` as `reference/golden/colby.schema.sql`, including FTS5 external-content config and the three trigger bodies.
