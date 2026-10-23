use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use codegraph_core::types::{
    Edge, EdgeKind, FileRecord, Language, Node, NodeKind, ReferenceSubkind, UnresolvedRef,
};
use rusqlite::{
    Connection, OptionalExtension, Row, ToSql, TransactionBehavior, named_params, params,
};
use serde_json::Value;

use crate::connection::Store;

pub(crate) const SQLITE_PARAM_CHUNK_SIZE: usize = 500;

/// Env var name (#1231): set to `1` to opt out of bulk-index WAL-checkpoint
/// deferral and keep SQLite's default `wal_autocheckpoint` interval.
pub const CODEGRAPH_NO_WAL_DEFER: &str = "CODEGRAPH_NO_WAL_DEFER";

/// Env var name (#1231): soft WAL-growth threshold in MB that triggers the
/// bulk-index [`Store::checkpoint_wal_if_over`] fold. Non-numeric / non-positive
/// falls back to [`DEFAULT_WAL_VALVE_MB`].
pub const CODEGRAPH_WAL_VALVE_MB: &str = "CODEGRAPH_WAL_VALVE_MB";

/// Default WAL-valve fold threshold in MB (#1231, mirrors upstream 256).
pub const DEFAULT_WAL_VALVE_MB: u64 = 256;

fn wal_defer_disabled() -> bool {
    matches!(std::env::var(CODEGRAPH_NO_WAL_DEFER), Ok(v) if v == "1")
}

/// Resolve the WAL-valve fold threshold in BYTES from `CODEGRAPH_WAL_VALVE_MB`
/// (#1231); non-numeric / non-positive → [`DEFAULT_WAL_VALVE_MB`].
pub fn wal_valve_threshold_bytes() -> u64 {
    let raw = std::env::var(CODEGRAPH_WAL_VALVE_MB).ok();
    wal_valve_threshold_bytes_from_raw(raw.as_deref())
}

fn wal_valve_threshold_bytes_from_raw(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|&mb| mb > 0)
        .and_then(|mb| mb.checked_mul(1024 * 1024))
        .filter(|&bytes| i64::try_from(bytes).is_ok())
        .unwrap_or(DEFAULT_WAL_VALVE_MB * 1024 * 1024)
}

#[cfg(test)]
mod wal_valve_threshold_tests {
    use super::{DEFAULT_WAL_VALVE_MB, wal_valve_threshold_bytes_from_raw};

    #[test]
    fn raw_threshold_resolver_accepts_positive_megabytes() {
        assert_eq!(wal_valve_threshold_bytes_from_raw(Some("1")), 1024 * 1024);
        assert_eq!(
            wal_valve_threshold_bytes_from_raw(Some("64")),
            64 * 1024 * 1024
        );
    }

    #[test]
    fn raw_threshold_resolver_falls_back_for_invalid_values() {
        let fallback = DEFAULT_WAL_VALVE_MB * 1024 * 1024;
        for raw in [None, Some(""), Some("invalid"), Some("0")] {
            assert_eq!(wal_valve_threshold_bytes_from_raw(raw), fallback);
        }
        let overflow = u64::MAX.to_string();
        assert_eq!(
            wal_valve_threshold_bytes_from_raw(Some(&overflow)),
            fallback
        );
        let sqlite_integer_overflow = (i64::MAX as u64 / (1024 * 1024) + 1).to_string();
        assert_eq!(
            wal_valve_threshold_bytes_from_raw(Some(&sqlite_integer_overflow)),
            fallback
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCounts {
    pub node_count: i64,
    pub edge_count: i64,
    pub file_count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub node: Node,
    pub score: f64,
}

/// One complete source-site group for reference refresh. A site can own several
/// sibling refs and edges, so these coordinates are a correlation key for the
/// whole group rather than an identity for one reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReferenceSite {
    pub from_node_id: String,
    pub line: i64,
    pub col: i64,
}

/// A source file and either one precise reference site or a whole-file fallback.
/// `site = None` means at least one historical non-`contains` edge had a NULL
/// line or column, so a caller cannot safely select a narrower group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileReferenceSite {
    pub file_path: String,
    pub site: Option<ReferenceSite>,
}

impl Store {
    /// Ports `insertNode` / `insertNodes` / `updateNode` from `upstream db/queries.ts:243-382`.
    /// Uses one batched transaction and an upsert that fires the FTS update trigger on conflicts.
    pub fn upsert_nodes(&mut self, nodes: &[Node]) -> rusqlite::Result<()> {
        validate_nodes(nodes)?;
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO nodes (
                  id, kind, name, qualified_name, file_path, language,
                  start_line, end_line, start_column, end_column,
                  docstring, signature, visibility,
                  is_exported, is_async, is_static, is_abstract,
                  decorators, type_parameters, return_type, updated_at
                ) VALUES (
                  @id, @kind, @name, @qualifiedName, @filePath, @language,
                  @startLine, @endLine, @startColumn, @endColumn,
                  @docstring, @signature, @visibility,
                  @isExported, @isAsync, @isStatic, @isAbstract,
                  @decorators, @typeParameters, @returnType, @updatedAt
                )
                ON CONFLICT(id) DO UPDATE SET
                  kind = excluded.kind,
                  name = excluded.name,
                  qualified_name = excluded.qualified_name,
                  file_path = excluded.file_path,
                  language = excluded.language,
                  start_line = excluded.start_line,
                  end_line = excluded.end_line,
                  start_column = excluded.start_column,
                  end_column = excluded.end_column,
                  docstring = excluded.docstring,
                  signature = excluded.signature,
                  visibility = excluded.visibility,
                  is_exported = excluded.is_exported,
                  is_async = excluded.is_async,
                  is_static = excluded.is_static,
                  is_abstract = excluded.is_abstract,
                  decorators = excluded.decorators,
                  type_parameters = excluded.type_parameters,
                  return_type = excluded.return_type,
                  updated_at = excluded.updated_at
                "#,
            )?;

            for node in nodes {
                let decorators = json_array_or_null(&node.decorators)?;
                let type_parameters = json_array_or_null(&node.type_parameters)?;
                stmt.execute(named_params! {
                    "@id": node.id,
                    "@kind": node.kind.as_str(),
                    "@name": node.name,
                    "@qualifiedName": node.qualified_name,
                    "@filePath": node.file_path,
                    "@language": node.language.as_str(),
                    "@startLine": node.start_line,
                    "@endLine": node.end_line,
                    "@startColumn": node.start_column,
                    "@endColumn": node.end_column,
                    "@docstring": node.docstring,
                    "@signature": node.signature,
                    "@visibility": node.visibility,
                    "@isExported": bool_to_i64(node.is_exported),
                    "@isAsync": bool_to_i64(node.is_async),
                    "@isStatic": bool_to_i64(node.is_static),
                    "@isAbstract": bool_to_i64(node.is_abstract),
                    "@decorators": decorators,
                    "@typeParameters": type_parameters,
                    "@returnType": node.return_type,
                    "@updatedAt": node.updated_at,
                })?;
            }
        }
        tx.commit()
    }

    /// Ports `deleteNode` from `upstream db/queries.ts:384-394`.
    pub fn delete_node(&self, id: &str) -> rusqlite::Result<usize> {
        self.conn.execute("DELETE FROM nodes WHERE id = ?", [id])
    }

    /// Ports `deleteNodesByFile` from `upstream db/queries.ts:396-410`.
    /// Edge cleanup is delegated to the schema's `ON DELETE CASCADE` foreign keys.
    pub fn delete_nodes_by_file_path(&self, file_path: &str) -> rusqlite::Result<usize> {
        self.conn
            .execute("DELETE FROM nodes WHERE file_path = ?", [file_path])
    }

    /// Ports `getNodeById` from `upstream db/queries.ts:412-436`.
    pub fn node_by_id(&self, id: &str) -> rusqlite::Result<Option<Node>> {
        self.conn
            .query_row("SELECT * FROM nodes WHERE id = ?", [id], row_to_node)
            .optional()
    }

    /// Ports `getNodesByFile` from `upstream db/queries.ts:530-541`.
    pub fn nodes_by_file_path(&self, file_path: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE file_path = ? ORDER BY start_line",
            [file_path],
        )
    }

    /// The functions and methods of `file_path`, by start line: the file's
    /// rows of [`Self::nodes_by_file_path`] of those kinds (W5-08). The `+`
    /// keeps the kind index out of the plan, so the file's own index walks
    /// its rows, as it does for every node of the file.
    pub fn callables_by_file_path(&self, file_path: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE file_path = ? AND +kind IN ('function', 'method') \
             ORDER BY start_line",
            [file_path],
        )
    }

    /// The methods of `file_path` in the order [`Self::nodes_by_file_path`]
    /// lists them: the same walk of the file's own index, the other rows
    /// skipped (W5-08).
    pub fn methods_by_file_path(&self, file_path: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE file_path = ? AND +kind = 'method' ORDER BY start_line",
            [file_path],
        )
    }

    /// Whether a function or method has the qualified name `qualified`
    /// (W5-08): the qualified-name index's rows, none of them read.
    pub fn has_callable_named(&self, qualified: &str) -> rusqlite::Result<bool> {
        self.conn
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM nodes WHERE qualified_name = ?1 \
                 AND +kind IN ('function', 'method'))",
            )?
            .query_row([qualified], |row| row.get(0))
    }

    /// Count of nodes whose `file_path` matches, for the displayed file-level
    /// symbol total. Reads the live `nodes` table (which includes framework
    /// marker nodes added after the initial extractor), so it can exceed the
    /// stored `files.node_count`; it never writes that column.
    pub fn node_count_by_file_path(&self, path: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE file_path = ?1",
            [path],
            |row| row.get(0),
        )
    }

    /// Ports `getNodesByKind` from `upstream db/queries.ts:695-704`.
    pub fn nodes_by_kind(&self, kind: NodeKind) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE kind = ?",
            [kind.as_str()],
        )
    }

    /// Ports `getNodesByName` from `upstream db/queries.ts:730-739`.
    pub fn nodes_by_name(&self, name: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(&self.conn, "SELECT * FROM nodes WHERE name = ?", [name])
    }

    /// Ports `getNodesByLowerName` from `upstream db/queries.ts:754-765`.
    /// The SQL expression mirrors `idx_nodes_lower_name`, so SQLite performs an
    /// index seek. `lower(?)` keeps parameter folding on SQLite's ASCII rules
    /// and does not require callers to lowercase first.
    pub fn nodes_by_lower_name(&self, name: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE lower(name) = lower(?)",
            [name],
        )
    }

    /// Ports `getNodesByQualifiedNameExact` from `upstream db/queries.ts:741-752`.
    pub fn nodes_by_qualified_name(&self, qualified_name: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE qualified_name = ?",
            [qualified_name],
        )
    }

    /// The nodes whose qualified name starts with `prefix` (`server::` for
    /// the members of `server`), read as an index range so the
    /// `qualified_name` index serves it.
    pub fn nodes_with_qualified_prefix(&self, prefix: &str) -> rusqlite::Result<Vec<Node>> {
        let mut upper = prefix.to_string();
        match upper.pop() {
            Some(last) => match char::from_u32(u32::from(last) + 1) {
                Some(next) => upper.push(next),
                None => return Ok(Vec::new()),
            },
            None => return Ok(Vec::new()),
        }
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE qualified_name >= ?1 AND qualified_name < ?2 ORDER BY id",
            [prefix, upper.as_str()],
        )
    }

    /// Ports `getNodesByIds` from `upstream db/queries.ts:453-488`.
    /// Batch node lookup keyed by id, chunked under SQLite's parameter limit
    /// (`SELECT * FROM nodes WHERE id IN (...)`), returned as an id -> node map.
    /// The upstream per-connection LRU cache is intentionally omitted — the Rust
    /// `Store` does no node caching, so this is the cache-miss SQL path only.
    pub fn nodes_by_ids(&self, ids: &[String]) -> rusqlite::Result<HashMap<String, Node>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        // upstream db/queries.ts:494 — dedup before chunking.
        let mut unique = ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT * FROM nodes WHERE id IN ({placeholders})");
            let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_node)?;
            for node in rows {
                let node = node?;
                out.insert(node.id.clone(), node);
            }
        }
        Ok(out)
    }

    /// The subset of `paths` whose file row has `generated = 1`. Batched and
    /// chunked like [`Self::nodes_by_ids`]; a path with no row is simply absent
    /// from the result.
    ///
    /// Reports ONLY what is stored: the union with the path convention
    /// (`is_generated_file`) is the CALLER's job, so this stays a faithful view
    /// of the column and the partial `idx_files_generated` index serves it.
    pub fn generated_paths_among(&self, paths: &[String]) -> rusqlite::Result<HashSet<String>> {
        let mut out = HashSet::new();
        if paths.is_empty() {
            return Ok(out);
        }
        let mut unique = paths.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT path FROM files WHERE generated = 1 AND path IN ({placeholders})");
            let params = chunk.iter().map(|p| p as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
            for path in rows {
                out.insert(path?);
            }
        }
        Ok(out)
    }

    /// Ports the FTS SQL from `searchNodesFTS` in `upstream db/queries.ts:986-1052`.
    /// Keeps the upstream FTS5 escaping/prefix rules, BM25 weights, `ORDER BY score LIMIT/OFFSET`.
    pub fn search_nodes_fts(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> rusqlite::Result<Vec<SearchResult>> {
        let fts_query = fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let fts_limit = std::cmp::max(limit * 5, 100);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT nodes.*, bm25(nodes_fts, 0, 20, 5, 1, 2) as score
            FROM nodes_fts
            JOIN nodes ON nodes_fts.id = nodes.id
            WHERE nodes_fts MATCH ?
            ORDER BY score LIMIT ? OFFSET ?
            "#,
        )?;
        let rows = stmt.query_map(params![fts_query, fts_limit, offset], |row| {
            Ok(SearchResult {
                node: row_to_node(row)?,
                score: row.get::<_, f64>("score")?.abs(),
            })
        })?;
        rows.collect()
    }

    /// Ports the full `searchNodesFTS` shape from `upstream db/queries.ts:989-1052`,
    /// including the optional `kind`/`language` `IN (...)` filters that the 3-arg
    /// [`Store::search_nodes_fts`] omits. Keeps the upstream FTS5 escaping/prefix rules,
    /// BM25 weights `bm25(nodes_fts, 0, 20, 5, 1, 2)`, and `ORDER BY score LIMIT/OFFSET`.
    pub fn search_nodes_fts_filtered(
        &self,
        query: &str,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: i64,
        offset: i64,
    ) -> rusqlite::Result<Vec<SearchResult>> {
        let fts_query = fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        // upstream db/queries.ts:1018 — Math.max(limit * 5, 100).
        let fts_limit = std::cmp::max(limit * 5, 100);

        let mut sql = String::from(
            r#"
            SELECT nodes.*, bm25(nodes_fts, 0, 20, 5, 1, 2) as score
            FROM nodes_fts
            JOIN nodes ON nodes_fts.id = nodes.id
            WHERE nodes_fts MATCH ?
            "#,
        );
        let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(fts_query)];
        append_kind_language_filters(
            &mut sql,
            &mut params,
            "nodes.kind",
            "nodes.language",
            kinds,
            languages,
        );
        // upstream db/queries.ts:1039 — ORDER BY score LIMIT ? OFFSET ?.
        sql.push_str(" ORDER BY score LIMIT ? OFFSET ?");
        params.push(Box::new(fts_limit));
        params.push(Box::new(offset));

        // upstream db/queries.ts:1048-1051 — a failed FTS query returns empty.
        let mut stmt = match self.conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(_) => return Ok(Vec::new()),
        };
        let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();
        let mapped = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(SearchResult {
                node: row_to_node(row)?,
                // upstream db/queries.ts:1046 — bm25 returns negative scores.
                score: row.get::<_, f64>("score")?.abs(),
            })
        });
        match mapped {
            Ok(rows) => rows.collect(),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Ports `searchNodesLike` from `upstream db/queries.ts:1058-1112`.
    /// Substring/prefix `LIKE` fallback used when FTS returns nothing; keeps the exact
    /// CASE score ladder and `ORDER BY score DESC, length(name) ASC LIMIT ? OFFSET ?`.
    pub fn search_nodes_like(
        &self,
        query: &str,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: i64,
        offset: i64,
    ) -> rusqlite::Result<Vec<SearchResult>> {
        let mut sql = String::from(
            r#"
            SELECT nodes.*,
              CASE
                WHEN name = ? THEN 1.0
                WHEN name LIKE ? THEN 0.9
                WHEN name LIKE ? THEN 0.8
                WHEN qualified_name LIKE ? THEN 0.7
                ELSE 0.5
              END as score
            FROM nodes
            WHERE (
              name LIKE ? OR
              qualified_name LIKE ? OR
              name LIKE ?
            )
            "#,
        );
        // upstream db/queries.ts:1079-1091 — exact, startsWith, contains variants.
        let exact_match = query.to_string();
        let starts_with = format!("{query}%");
        let contains = format!("%{query}%");
        let mut params: Vec<Box<dyn ToSql>> = vec![
            Box::new(exact_match),
            Box::new(starts_with.clone()),
            Box::new(contains.clone()),
            Box::new(contains.clone()),
            Box::new(contains.clone()),
            Box::new(contains),
            Box::new(starts_with),
        ];
        append_kind_language_filters(&mut sql, &mut params, "kind", "language", kinds, languages);
        // upstream db/queries.ts:1103 — ORDER BY score DESC, length(name) ASC.
        sql.push_str(" ORDER BY score DESC, length(name) ASC LIMIT ? OFFSET ?");
        params.push(Box::new(limit));
        params.push(Box::new(offset));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(SearchResult {
                node: row_to_node(row)?,
                score: row.get::<_, f64>("score")?,
            })
        })?;
        rows.collect()
    }

    /// Ports `searchAllByFilters` from `upstream db/queries.ts:900-920`.
    /// Filter-only candidate set (no FTS text); `ORDER BY name LIMIT ?`, each scored `1`.
    pub fn search_all_by_filters(
        &self,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: i64,
    ) -> rusqlite::Result<Vec<SearchResult>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE 1=1");
        let mut params: Vec<Box<dyn ToSql>> = Vec::new();
        append_kind_language_filters(&mut sql, &mut params, "kind", "language", kinds, languages);
        // upstream db/queries.ts:916 — ORDER BY name LIMIT ?.
        sql.push_str(" ORDER BY name LIMIT ?");
        params.push(Box::new(limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(SearchResult {
                node: row_to_node(row)?,
                // upstream db/queries.ts:919 — score: 1.
                score: 1.0,
            })
        })?;
        rows.collect()
    }

    /// Ports the exact-name supplement query from `upstream db/queries.ts:834-845`.
    /// `lower(name) = lower(?)` plus optional kind/language `IN (...)`;
    /// `LIMIT 20`. The matching expression pins the planner to
    /// `idx_nodes_lower_name`, including misses.
    pub fn nodes_by_exact_name_nocase(
        &self,
        name: &str,
        kinds: &[NodeKind],
        languages: &[Language],
    ) -> rusqlite::Result<Vec<Node>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE lower(name) = lower(?)");
        let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(name.to_string())];
        append_kind_language_filters(&mut sql, &mut params, "kind", "language", kinds, languages);
        // upstream db/queries.ts:844 — LIMIT 20.
        sql.push_str(" LIMIT 20");
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), row_to_node)?;
        rows.collect()
    }

    /// Ports the fuzzy follow-up query from `upstream db/queries.ts:962-972`.
    /// `name = ?` plus optional kind/language `IN (...)`; `LIMIT 5`.
    pub fn nodes_by_exact_name_filtered(
        &self,
        name: &str,
        kinds: &[NodeKind],
        languages: &[Language],
    ) -> rusqlite::Result<Vec<Node>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE name = ?");
        let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(name.to_string())];
        append_kind_language_filters(&mut sql, &mut params, "kind", "language", kinds, languages);
        // upstream db/queries.ts:972 — LIMIT 5.
        sql.push_str(" LIMIT 5");
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), row_to_node)?;
        rows.collect()
    }

    /// Callable/variable/constant definitions whose name CONTAINS `needle` as a
    /// substring, fetched as a SEPARATE batch from the FTS/LIKE ladder
    /// (upstream `1de7e8f` #1319 plus v1.6.0 variable seeding).
    ///
    /// `needle` is matched lowercase against both `lower(name)` and its
    /// separator-stripped form, so a `user_profile_id` query reaches a
    /// `userProfileId`-humped name and a `userProfileId` query reaches a
    /// `user_profile_id`-separated one. Substring containment is only the
    /// CANDIDATE gate; the caller still applies the segment-boundary filter, so
    /// an incidental namesake never becomes a match. `ORDER BY` is fully
    /// specified — shortest name first, then name/path/line — so the candidate
    /// order never depends on SQLite's incidental row order.
    pub fn seedable_nodes_by_name_infix(
        &self,
        needle: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<Node>> {
        let pattern = format!("%{}%", needle.to_lowercase());
        let mut stmt = self.conn.prepare(
            r#"
            SELECT * FROM nodes
            WHERE kind IN ('function', 'method', 'component', 'variable', 'constant')
              AND (
                lower(name) LIKE ?
                OR replace(replace(replace(lower(name), '_', ''), '-', ''), '.', '') LIKE ?
              )
            ORDER BY length(name) ASC, name ASC, file_path ASC, start_line ASC
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(params![pattern, pattern, limit], row_to_node)?;
        rows.collect()
    }

    /// Whether any node's name, qualified name, signature or docstring holds an
    /// FTS token starting with `word` — the posting-list half of upstream's
    /// `getExploreMissDiagnostics` (#1904). `word` must be letters/digits only.
    pub fn fts_any_column_has_prefix(&self, word: &str) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM nodes_fts WHERE nodes_fts MATCH ?1)",
            params![format!(
                "{{name qualified_name signature docstring}} : \"{word}\"*"
            )],
            |row| row.get::<_, bool>(0),
        )
    }

    /// Up to `limit` names of non-file, non-import nodes whose NAME holds an FTS
    /// token starting with one of `words` (letters/digits only), in index order
    /// (#1904's retry candidates).
    pub fn fts_name_prefix_names(
        &self,
        words: &[String],
        limit: usize,
    ) -> rusqlite::Result<Vec<String>> {
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!(
            "name : ({})",
            words
                .iter()
                .map(|word| format!("\"{word}\"*"))
                .collect::<Vec<_>>()
                .join(" OR ")
        );
        let mut stmt = self.conn.prepare(
            "SELECT n.name FROM nodes_fts JOIN nodes n ON n.rowid = nodes_fts.rowid \
             WHERE nodes_fts MATCH ?1 AND n.kind NOT IN ('file', 'import') \
             ORDER BY nodes_fts.rowid LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect()
    }

    /// Ports `getAllNodeNames` from `upstream db/queries.ts:1655-1661`.
    /// `SELECT DISTINCT name FROM nodes` — the candidate name set for fuzzy fallback.
    pub fn all_node_names(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT name FROM nodes")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>("name"))?;
        rows.collect()
    }

    /// Distinct segmentable node names (kind is neither `file` nor `import`) —
    /// the query-time source for the prompt-hook MEDIUM tier, the golden-neutral
    /// substitution for upstream's index-time `name_segment_vocab` table (#1136,
    /// #1144 `isSegmentableKind`). `file`/`import` are excluded (no prose signal;
    /// can't be surfaced as a real symbol). `ORDER BY name` keeps it
    /// deterministic; the `MIN(kind)` representative is advisory (the caller
    /// re-verifies via [`Store::nodes_by_name`]).
    pub fn distinct_non_file_node_names(&self) -> rusqlite::Result<Vec<(String, NodeKind)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, MIN(kind) AS kind FROM nodes \
             WHERE kind != 'file' AND kind != 'import' \
             GROUP BY name ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            let name = row.get::<_, String>("name")?;
            let kind = parse_node_kind(row.get::<_, String>("kind")?)?;
            Ok((name, kind))
        })?;
        rows.collect()
    }

    /// Ports `insertEdge` / `insertEdges` from `upstream db/queries.ts:1255-1298`.
    /// Inserts in one transaction, skips edges whose endpoints are absent, and uses `INSERT OR IGNORE`.
    pub fn insert_edges(&mut self, edges: &[Edge]) -> rusqlite::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let endpoint_ids = edges
            .iter()
            .flat_map(|edge| [edge.source.as_str(), edge.target.as_str()])
            .collect::<Vec<_>>();

        // Snapshot endpoint existence INSIDE a `BEGIN IMMEDIATE` transaction so the
        // FK filter and the inserts observe one write-locked, delete-free view.
        // Reading it before the transaction let a concurrent writer (daemon
        // catch-up sync on one connection, watcher sync on another) delete an
        // endpoint between the snapshot and the insert: the edge passed the stale
        // filter but tripped `FOREIGN KEY constraint failed`, aborting the sync.
        // Dropping a genuinely-absent endpoint stays byte-identical to before.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_node_ids = existing_node_ids(&tx, &endpoint_ids)?;
        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance)
                VALUES (@source, @target, @kind, @metadata, @line, @col, @provenance)
                "#,
            )?;
            for edge in edges {
                if !existing_node_ids.contains(&edge.source)
                    || !existing_node_ids.contains(&edge.target)
                {
                    continue;
                }
                let metadata = edge
                    .metadata
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(json_to_sql_error)?;
                stmt.execute(named_params! {
                    "@source": edge.source,
                    "@target": edge.target,
                    "@kind": edge.kind.as_str(),
                    "@metadata": metadata,
                    "@line": edge.line,
                    "@col": edge.col,
                    "@provenance": edge.provenance,
                })?;
            }
        }
        tx.commit()
    }

    /// Ports `getOutgoingEdges` from `upstream db/queries.ts:1310-1337`.
    pub fn edges_by_source_kind(
        &self,
        source_id: &str,
        kind: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<Edge>> {
        match kind {
            Some(kind) => query_edges(
                &self.conn,
                "SELECT * FROM edges WHERE source = ? AND kind = ?",
                params![source_id, kind.as_str()],
            ),
            None => query_edges(
                &self.conn,
                "SELECT * FROM edges WHERE source = ?",
                params![source_id],
            ),
        }
    }

    /// Every edge of `kind`, ordered by row id.
    ///
    /// This is the bulk counterpart to [`Self::edges_by_source_kind`]. Callers
    /// that need one graph-wide edge class should issue one indexed query rather
    /// than one query per source node.
    pub fn edges_by_kind(&self, kind: EdgeKind) -> rusqlite::Result<Vec<Edge>> {
        query_edges(
            &self.conn,
            "SELECT * FROM edges WHERE kind = ? ORDER BY id",
            [kind.as_str()],
        )
    }

    /// Ports `getIncomingEdges` from `upstream db/queries.ts:1339-1354`.
    pub fn edges_by_target_kind(
        &self,
        target_id: &str,
        kind: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<Edge>> {
        match kind {
            Some(kind) => query_edges(
                &self.conn,
                "SELECT * FROM edges WHERE target = ? AND kind = ?",
                params![target_id, kind.as_str()],
            ),
            None => query_edges(
                &self.conn,
                "SELECT * FROM edges WHERE target = ?",
                params![target_id],
            ),
        }
    }

    /// Ports `upsertFile` from `upstream db/queries.ts:1426-1455`.
    pub fn upsert_file(&self, file: &FileRecord) -> rusqlite::Result<usize> {
        let errors = json_array_or_null(&file.errors)?;
        self.conn.execute(
            r#"
            INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors, generated)
            VALUES (@path, @contentHash, @language, @size, @modifiedAt, @indexedAt, @nodeCount, @errors, @generated)
            ON CONFLICT(path) DO UPDATE SET
              content_hash = @contentHash,
              language = @language,
              size = @size,
              modified_at = @modifiedAt,
              indexed_at = @indexedAt,
              node_count = @nodeCount,
              errors = @errors,
              generated = @generated
            "#,
            named_params! {
                "@path": file.path,
                "@contentHash": file.content_hash,
                "@language": file.language.as_str(),
                "@size": file.size,
                "@modifiedAt": file.modified_at,
                "@indexedAt": file.indexed_at,
                "@nodeCount": file.node_count,
                "@errors": errors,
                "@generated": i64::from(file.generated),
            },
        )
    }

    /// Ports `deleteFile` from `upstream db/queries.ts:1457-1468`.
    /// Deletes nodes first so FK cascades remove their edges, then deletes the file row.
    pub fn delete_file_record(&mut self, file_path: &str) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM nodes WHERE file_path = ?", [file_path])?;
        tx.execute("DELETE FROM files WHERE path = ?", [file_path])?;
        tx.commit()
    }

    /// Ports `getFileByPath` from `upstream db/queries.ts:1470-1479`.
    pub fn file_by_path(&self, file_path: &str) -> rusqlite::Result<Option<FileRecord>> {
        self.conn
            .query_row(
                "SELECT * FROM files WHERE path = ?",
                [file_path],
                row_to_file,
            )
            .optional()
    }

    /// Ports `getAllFiles` from `upstream db/queries.ts:1481-1490`.
    pub fn all_files(&self) -> rusqlite::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare("SELECT * FROM files ORDER BY path")?;
        let rows = stmt.query_map([], row_to_file)?;
        rows.collect()
    }

    /// The paths of the files of `language`, in path order (W4-DART-03b: a
    /// sync lane reading every Dart file's text needs no other column).
    pub fn file_paths_of_language(&self, language: Language) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM files WHERE language = ?1 ORDER BY path")?;
        let rows = stmt.query_map([language.as_str()], |row| row.get(0))?;
        rows.collect()
    }

    /// Files stored with no nodes or with recorded extraction errors: the only
    /// rows that can be missing their symbols (upstream #2336's
    /// `getFilesWithoutNodesOrWithErrors`). A healthy index has few, so this
    /// stays cheap on a large one.
    pub fn files_without_nodes_or_with_errors(&self) -> rusqlite::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM files WHERE node_count = 0 OR errors IS NOT NULL ORDER BY path",
        )?;
        let rows = stmt.query_map([], row_to_file)?;
        rows.collect()
    }

    /// Every node in the graph, ordered by id for a deterministic export.
    pub fn all_nodes(&self) -> rusqlite::Result<Vec<Node>> {
        query_nodes(&self.conn, "SELECT * FROM nodes ORDER BY id", [])
    }

    /// Every edge in the graph, ordered by rowid for a deterministic export.
    pub fn all_edges(&self) -> rusqlite::Result<Vec<Edge>> {
        query_edges(&self.conn, "SELECT * FROM edges ORDER BY id", [])
    }

    /// Ports `insertUnresolvedRef` / batch wrapper from `upstream db/queries.ts:1518-1552`.
    ///
    /// Skips refs whose `from_node_id` is absent from `nodes` (like
    /// [`Self::insert_edges`] does for endpoints): the column is an
    /// `ON DELETE CASCADE` FK to `nodes(id)`, and a node-id collision during
    /// `upsert_nodes` (two distinct nodes hashing to the same id) can leave a
    /// ref pointing at an id that never materialized — inserting it raises
    /// `FOREIGN KEY constraint failed` and aborts the whole index. An orphan ref
    /// could never resolve anyway, so dropping it is golden-neutral (the
    /// equivalence oracle does not compare `unresolved_refs`).
    pub fn insert_unresolved_refs(&mut self, refs: &[UnresolvedRef]) -> rusqlite::Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        // Snapshot `from_node_id` existence INSIDE a `BEGIN IMMEDIATE` transaction
        // (see `insert_edges`): a concurrent writer deleting the source node
        // between an out-of-transaction snapshot and the insert would trip
        // `FOREIGN KEY constraint failed` and abort the sync. Dropping a
        // genuinely-absent source stays byte-identical to before.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_unresolved_refs_in_connection(&tx, refs)?;
        tx.commit()
    }

    /// Ports `getUnresolvedReferences` from `upstream db/queries.ts:1588-1603`.
    pub fn all_unresolved_refs(&self) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare("SELECT * FROM unresolved_refs")?;
        let rows = stmt.query_map([], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// Every unresolved reference of `kind`, in row order.
    pub fn unresolved_refs_of_kind(&self, kind: EdgeKind) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM unresolved_refs WHERE reference_kind = ? ORDER BY id")?;
        let rows = stmt.query_map([kind.as_str()], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// Read one rowid-ordered batch of unresolved references with `id > after_id`.
    ///
    /// `id` is the rowid alias (`INTEGER PRIMARY KEY AUTOINCREMENT`), so
    /// `WHERE id > ? ORDER BY id` walks the SAME ascending order that
    /// `all_unresolved_refs`'s bare `SELECT *` returns implicitly. Advancing the
    /// cursor by the last id seen steps past retained (unresolvable) rows without
    /// deleting them, so the final `unresolved_refs` set equals the all-at-once pass.
    pub fn unresolved_refs_batch(
        &self,
        after_id: i64,
        limit: usize,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id > ? ORDER BY id LIMIT ?")?;
        let rows = stmt.query_map(params![after_id, limit as i64], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// One rowid-ordered batch of the unresolved references with `id >
    /// after_id` whose kind is one of `kinds` (`within`) or none of them.
    pub fn unresolved_refs_batch_by_kinds(
        &self,
        after_id: i64,
        limit: usize,
        kinds: &[EdgeKind],
        within: bool,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let listed = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT * FROM unresolved_refs WHERE id > ? AND (reference_kind IN ({listed})) = ? \
             ORDER BY id LIMIT ?"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut values: Vec<rusqlite::types::Value> = vec![after_id.into()];
        values.extend(kinds.iter().map(|kind| kind.as_str().to_string().into()));
        values.push(within.into());
        values.push((limit as i64).into());
        let rows = stmt.query_map(rusqlite::params_from_iter(values), row_to_unresolved_ref)?;
        rows.collect()
    }

    /// Count rows currently in `unresolved_refs`. Sizes the resolve-phase
    /// progress bar; read AFTER framework refs are inserted or the bar overflows.
    pub fn unresolved_refs_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT count(*) FROM unresolved_refs", [], |row| row.get(0))
    }

    /// Ports `deleteSpecificResolvedReferences` from `upstream db/queries.ts:1716-1727`
    /// (row-id form, #1269/#1270 / upstream `e871c49`).
    ///
    /// Deletes EXACTLY the rows named by `row_ids` in a single transaction. `id` is
    /// the table's `INTEGER PRIMARY KEY AUTOINCREMENT`, so each id addresses one
    /// row and nothing else.
    ///
    /// The previous key — `(from_node_id, reference_name, reference_kind)` — is NOT
    /// unique: two calls to the same name from the same enclosing node differ only
    /// in `line`/`col`/`id`, and the tuple-keyed `DELETE` carried no `LIMIT`, so
    /// resolving ONE of them deleted BOTH and the sibling's edge was silently lost.
    /// Keying on the row id removes that whole class of loss, and with it the need
    /// for any `id <= max_id` batch guard: an id from batch N cannot name a row in
    /// batch N+1, so a per-batch caller is already precise.
    ///
    /// Each row moves whole into `resolved_refs`, keeping its id (schema 10,
    /// W5-09e): with the rows still unresolved, they stay every reference of
    /// the file's last extraction. The ids move a chunk at a time, one
    /// statement each way (W5-09g); an id listed twice moves once, since its
    /// first move deleted the row the second would select.
    pub fn delete_resolved_unresolved_refs(&mut self, row_ids: &[i64]) -> rusqlite::Result<()> {
        if row_ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for chunk in row_ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
            tx.prepare_cached(&format!(
                "INSERT OR IGNORE INTO resolved_refs ({UNRESOLVED_REF_COLUMNS}) \
                 SELECT {UNRESOLVED_REF_COLUMNS} FROM unresolved_refs WHERE id IN ({placeholders})"
            ))?
            .execute(params.as_slice())?;
            tx.prepare_cached(&format!(
                "DELETE FROM unresolved_refs WHERE id IN ({placeholders})"
            ))?
            .execute(params.as_slice())?;
        }
        tx.commit()
    }

    /// [`Self::stored_refs_of_file`] at `sites` only: every stored reference
    /// whose source and coordinate are one of them, by id and without it.
    pub fn stored_refs_at_sites(
        &self,
        sites: &BTreeSet<ReferenceSite>,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {UNRESOLVED_REF_COLUMNS} FROM unresolved_refs \
             WHERE from_node_id = ?1 AND line = ?2 AND col = ?3 \
             UNION ALL SELECT {UNRESOLVED_REF_COLUMNS} FROM resolved_refs \
             WHERE from_node_id = ?1 AND line = ?2 AND col = ?3"
        ))?;
        let mut out = Vec::new();
        for site in sites {
            for reference in stmt.query_map(
                params![site.from_node_id, site.line, site.col],
                row_to_unresolved_ref,
            )? {
                out.push(reference?);
            }
        }
        out.sort_by_key(|reference| reference.id);
        for reference in &mut out {
            reference.id = None;
        }
        Ok(out)
    }

    /// Every reference of `file_path`'s last extraction, the rows resolution
    /// moved into `resolved_refs` and the ones still in `unresolved_refs`, by
    /// id and without it (W5-09e): what re-extracting the unchanged file would
    /// give, as each site's multiset of references. Only valid while the
    /// `resolved_refs_version` marker is.
    pub fn stored_refs_of_file(&self, file_path: &str) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {UNRESOLVED_REF_COLUMNS} FROM unresolved_refs WHERE file_path = ?1 \
             UNION ALL SELECT {UNRESOLVED_REF_COLUMNS} FROM resolved_refs WHERE file_path = ?1 \
             ORDER BY id"
        ))?;
        let rows = stmt.query_map([file_path], row_to_unresolved_ref)?;
        rows.map(|row| {
            row.map(|mut reference| {
                reference.id = None;
                reference
            })
        })
        .collect()
    }

    /// Ports `getUnresolvedReferencesByFiles` from `upstream db/queries.ts:1663-1694`.
    /// A single-file call uses the same `file_path IN (...)` shape as the upstream chunked path.
    pub fn unresolved_refs_by_file_path(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM unresolved_refs WHERE file_path IN (?)")?;
        let rows = stmt.query_map([file_path], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// Multi-file form of [`Store::unresolved_refs_by_file_path`] mirroring the
    /// chunked `file_path IN (...)` shape of the upstream `getUnresolvedReferencesByFiles`
    /// (`upstream db/queries.ts:1663-1694`). Backs the incremental
    /// resolve path during `sync`: only refs whose source file was re-extracted
    /// (changed files + their dependents) are re-resolved, instead of the whole
    /// `unresolved_refs` table. Results are deduplicated by the file set and
    /// returned in SQLite scan order per chunk.
    pub fn unresolved_refs_by_files(
        &self,
        file_paths: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut out = Vec::new();
        if file_paths.is_empty() {
            return Ok(out);
        }
        let mut unique = file_paths.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT * FROM unresolved_refs WHERE file_path IN ({placeholders})");
            let params = chunk.iter().map(|p| p as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_unresolved_ref)?;
            for reference in rows {
                out.push(reference?);
            }
        }
        Ok(out)
    }

    /// Read every persisted unresolved row in the selected source-site groups.
    /// Sites are sorted and deduplicated first; rows within each group are ordered
    /// by physical row id so sibling multiplicity is preserved deterministically.
    pub fn unresolved_refs_by_sites(
        &self,
        sites: &[ReferenceSite],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let unique = sites.iter().cloned().collect::<BTreeSet<_>>();
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare_cached(
            r#"SELECT * FROM unresolved_refs
            WHERE from_node_id = ?1 AND line = ?2 AND col = ?3
            ORDER BY id"#,
        )?;
        for site in unique {
            let rows = stmt.query_map(
                params![site.from_node_id, site.line, site.col],
                row_to_unresolved_ref,
            )?;
            for reference in rows {
                out.push(reference?);
            }
        }
        Ok(out)
    }

    /// Delete every persisted unresolved sibling in the selected source-site
    /// groups. No target-name or edge-kind predicate may split a site group.
    pub fn delete_unresolved_refs_at_sites(
        &mut self,
        sites: &[ReferenceSite],
    ) -> rusqlite::Result<usize> {
        let unique = sites.iter().cloned().collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        {
            let mut stmt = tx.prepare_cached(
                r#"DELETE FROM unresolved_refs
                WHERE from_node_id = ?1 AND line = ?2 AND col = ?3"#,
            )?;
            for site in unique {
                removed += stmt.execute(params![site.from_node_id, site.line, site.col])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Fetch unresolved references whose `reference_name` is in `names`, chunked
    /// under SQLite's parameter limit (`reference_name IN (...)`, backed by
    /// `idx_unresolved_name`). Used by the incremental resolve path to cover the
    /// equivalence danger case: a ref in an UNCHANGED file that was previously
    /// unresolved but should now resolve because a changed file added/removed a
    /// symbol with that name. Returns rows in SQLite scan order per chunk.
    pub fn unresolved_refs_by_names(
        &self,
        names: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut out = Vec::new();
        if names.is_empty() {
            return Ok(out);
        }
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT * FROM unresolved_refs WHERE reference_name IN ({placeholders})");
            let params = chunk.iter().map(|p| p as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_unresolved_ref)?;
            for reference in rows {
                out.push(reference?);
            }
        }
        Ok(out)
    }

    /// The unresolved references of `language`, in id order: a lane whose
    /// language writes tails the shared tail lookup cannot read (an
    /// Objective-C keyword selector, `cache.storeImage:forKey:`) filters them
    /// itself.
    pub fn unresolved_refs_of_language(
        &self,
        language: Language,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM unresolved_refs WHERE language = ?1 ORDER BY id")?;
        let rows = stmt.query_map([language.as_str()], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// The files of `language` whose `extends` or `implements` references
    /// name `name`, resolved (to a node of that name) or parked (W4-DART-02).
    pub fn files_with_inheritance_refs_to(
        &self,
        language: Language,
        name: &str,
    ) -> rusqlite::Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.file_path FROM edges e \
             JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target \
             WHERE e.kind IN ('extends', 'implements') AND t.name = ?2 AND s.language = ?1 \
             UNION SELECT file_path FROM unresolved_refs \
             WHERE reference_kind IN ('extends', 'implements') AND reference_name = ?2 \
             AND language = ?1",
        )?;
        stmt.query_map(params![language.as_str(), name], |row| row.get(0))?
            .collect()
    }

    /// The names `file`'s `extends` and `implements` references write: the
    /// target's name of a resolved one, the reference's own of a parked one
    /// (W4-DART-02).
    pub fn inheritance_ref_names_in_file(&self, file: &str) -> rusqlite::Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name FROM edges e \
             JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target \
             WHERE e.kind IN ('extends', 'implements') AND s.file_path = ?1 \
             UNION SELECT reference_name FROM unresolved_refs \
             WHERE reference_kind IN ('extends', 'implements') AND file_path = ?1",
        )?;
        stmt.query_map(params![file], |row| row.get(0))?.collect()
    }

    /// The unresolved references of `language` written with a receiver,
    /// `receiver.member`, whose member (the text after the last `.`) is one of
    /// `members`, in id order and with no ceiling: a lane that knows which
    /// members can have moved retries every call of them (W4-PHP-01). The
    /// match runs in SQL, so the language's other references are never read.
    pub fn unresolved_refs_of_language_by_members(
        &self,
        language: Language,
        members: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut unique: Vec<&str> = members
            .iter()
            .map(String::as_str)
            .filter(|member| !member.is_empty() && !member.contains('.'))
            .collect();
        unique.sort_unstable();
        unique.dedup();
        let mut out: BTreeMap<i64, UnresolvedRef> = BTreeMap::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE - 1) {
            let clauses = (0..chunk.len())
                .map(|index| format!("reference_name GLOB ?{}", index + 2))
                .collect::<Vec<_>>()
                .join(" OR ");
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE language = ?1 AND ({clauses}) ORDER BY id"
            );
            // `*`, `?` and `[` are GLOB's own; a bracket makes each literal.
            let patterns: Vec<String> = chunk
                .iter()
                .map(|member| {
                    let mut escaped = String::from("*.");
                    for c in member.chars() {
                        match c {
                            '*' => escaped.push_str("[*]"),
                            '?' => escaped.push_str("[?]"),
                            '[' => escaped.push_str("[[]"),
                            _ => escaped.push(c),
                        }
                    }
                    escaped
                })
                .collect();
            let language = language.as_str();
            let mut params: Vec<&dyn ToSql> = vec![&language];
            params.extend(patterns.iter().map(|p| p as &dyn ToSql));
            let mut stmt = self.conn.prepare(&sql)?;
            for reference in stmt.query_map(params.as_slice(), row_to_unresolved_ref)? {
                let reference = reference?;
                if let Some(id) = reference.id {
                    out.insert(id, reference);
                }
            }
        }
        Ok(out.into_values().collect())
    }

    /// The unresolved references written in `language` through one of
    /// `receivers`, named `<receiver>.<…>` (`Provider.autoDispose`), in id
    /// order: a sync retries those whose receiver's type came or went
    /// (W4-DART-03), which the tail lookup never names.
    pub fn unresolved_refs_of_language_through(
        &self,
        language: Language,
        receivers: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut unique: Vec<&str> = receivers
            .iter()
            .map(String::as_str)
            .filter(|receiver| !receiver.is_empty())
            .collect();
        unique.sort_unstable();
        unique.dedup();
        let mut out: BTreeMap<i64, UnresolvedRef> = BTreeMap::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE - 1) {
            let clauses = (0..chunk.len())
                .map(|index| format!("reference_name GLOB ?{}", index + 2))
                .collect::<Vec<_>>()
                .join(" OR ");
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE language = ?1 AND ({clauses}) ORDER BY id"
            );
            // `*`, `?` and `[` are GLOB's own; a bracket makes each literal.
            let patterns: Vec<String> = chunk
                .iter()
                .map(|receiver| {
                    let mut escaped = String::new();
                    for c in receiver.chars() {
                        match c {
                            '*' => escaped.push_str("[*]"),
                            '?' => escaped.push_str("[?]"),
                            '[' => escaped.push_str("[[]"),
                            _ => escaped.push(c),
                        }
                    }
                    escaped.push_str(".*");
                    escaped
                })
                .collect();
            let language = language.as_str();
            let mut params: Vec<&dyn ToSql> = vec![&language];
            params.extend(patterns.iter().map(|p| p as &dyn ToSql));
            let mut stmt = self.conn.prepare(&sql)?;
            for reference in stmt.query_map(params.as_slice(), row_to_unresolved_ref)? {
                let reference = reference?;
                if let Some(id) = reference.id {
                    out.insert(id, reference);
                }
            }
        }
        Ok(out.into_values().collect())
    }

    /// The unresolved references written in one of `languages`, whose names
    /// ignore case, named like one of `names` in any ASCII case (SQLite's
    /// `lower`, as the case-insensitive node lookup folds), in id order: a
    /// sync retries `\HELPER()` once a `helper` comes or goes (W4-PHP-02). No
    /// index folds case, so those languages' rows are scanned.
    pub fn unresolved_refs_by_folded_names(
        &self,
        languages: &[Language],
        names: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        if languages.is_empty() || names.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let languages: Vec<&str> = languages.iter().map(|language| language.as_str()).collect();
        let language_slots = std::iter::repeat_n("?", languages.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut out: BTreeMap<i64, UnresolvedRef> = BTreeMap::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE - languages.len()) {
            let name_slots = std::iter::repeat_n("lower(?)", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE language IN ({language_slots}) \
                 AND lower(reference_name) IN ({name_slots}) ORDER BY id"
            );
            let mut params: Vec<&dyn ToSql> = languages
                .iter()
                .map(|language| language as &dyn ToSql)
                .collect();
            params.extend(chunk.iter().map(|name| name as &dyn ToSql));
            let mut stmt = self.conn.prepare(&sql)?;
            for reference in stmt.query_map(params.as_slice(), row_to_unresolved_ref)? {
                let reference = reference?;
                if let Some(id) = reference.id {
                    out.insert(id, reference);
                }
            }
        }
        Ok(out.into_values().collect())
    }

    /// Sites of the surviving non-`contains` edges whose source is written in
    /// one of `languages` and whose target is named like one of `names` in
    /// any ASCII case, in either file relation: a case-insensitive language's
    /// fuzzy match keeps a target only while it is the one node of its name in
    /// any case, so a namesake that comes, goes or changes in another case
    /// moves it (W4-PHP-03).
    pub fn reference_sites_of_edges_to_folded_targets(
        &self,
        languages: &[Language],
        names: &[String],
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        if languages.is_empty() || names.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let languages: Vec<&str> = languages.iter().map(|language| language.as_str()).collect();
        let language_slots = std::iter::repeat_n("?", languages.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut out = BTreeSet::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE - languages.len()) {
            let name_slots = std::iter::repeat_n("lower(?)", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"SELECT src.file_path, e.source, e.line, e.col
                FROM edges e
                JOIN nodes tgt ON tgt.id = e.target
                JOIN nodes src ON src.id = e.source
                WHERE lower(tgt.name) IN ({name_slots})
                  AND src.language IN ({language_slots})
                  AND e.kind != 'contains'
                ORDER BY src.file_path, e.source, e.line, e.col"#
            );
            let mut params: Vec<&dyn ToSql> = chunk.iter().map(|name| name as &dyn ToSql).collect();
            params.extend(languages.iter().map(|language| language as &dyn ToSql));
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_file_reference_site)?;
            for row in rows {
                out.insert(row?);
            }
        }
        Ok(out.into_iter().collect())
    }

    /// The unresolved PHP calls through a class written with a namespace
    /// (`F\FirstName::make()`, recorded `F\FirstName.make`), in id order: a
    /// sync retries those through a class whose name came or went, with no
    /// ceiling (W4-PHP-02).
    pub fn unresolved_php_calls_through_qualified_classes(
        &self,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM unresolved_refs WHERE language = 'php' AND reference_kind = 'calls' \
             AND instr(reference_name, '\\') > 0 AND instr(reference_name, '.') > 0 ORDER BY id",
        )?;
        let rows = stmt.query_map([], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// The unresolved references written with a qualifier (`User.create`,
    /// `objectType.IsEnum`) whose [`reference_name_tail`] is one of `names`,
    /// the symbol names a sync's changed files touched (upstream #1240's
    /// `name_tail` retry). The whole-name lookup,
    /// [`Self::unresolved_refs_by_names`], never matches them. A tail more
    /// than `per_key_ceiling` such references share is skipped, as
    /// upstream's per-name ceiling skips names that common.
    pub fn unresolved_refs_by_name_tails(
        &self,
        names: &[String],
        per_key_ceiling: usize,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let names: HashSet<&str> = names.iter().map(String::as_str).collect();
        // Every dotted or colon reference's name and kind first, the rest of
        // a row read only for the ones chosen (W5-08): the same rows, the
        // same order.
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, reference_name, reference_kind FROM unresolved_refs \
             WHERE instr(reference_name, '.') > 0 OR instr(reference_name, ':') > 0 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let raw_kind = row.get::<_, String>(2)?;
                let kind = if raw_kind == "function_ref" {
                    EdgeKind::References
                } else {
                    parse_edge_kind(raw_kind)?
                };
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, kind))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, (_, name, kind)) in rows.iter().enumerate() {
            let tail = reference_name_tail(name, *kind);
            if tail != *name && names.contains(tail.as_str()) {
                buckets.entry(tail).or_default().push(index);
            }
        }
        let mut chosen = BTreeSet::new();
        for indexes in buckets.values() {
            if indexes.len() <= per_key_ceiling {
                chosen.extend(indexes.iter().copied());
            }
        }
        let mut by_id = self
            .conn
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id = ?1")?;
        chosen
            .into_iter()
            .map(|index| by_id.query_row([rows[index].0], row_to_unresolved_ref))
            .collect()
    }

    /// The parked references a sync retries by the names its changed files
    /// touched that the whole-name lookup ([`Self::unresolved_refs_by_names`])
    /// never matches, found in one pass over the table (W5-08), each row once
    /// in id order: a qualified reference by its tail, a tail more than
    /// `tail_ceiling` share skipped ([`Self::unresolved_refs_by_name_tails`]);
    /// a PHP call written `\helper()` by its name, and a name of
    /// `folded_languages`, in any ASCII case
    /// ([`Self::unresolved_refs_by_folded_names`]); and a PHP call through a
    /// class written with a namespace whose class or member is one of the
    /// names ([`Self::unresolved_php_calls_through_qualified_classes`]).
    pub fn unresolved_refs_retried_by_names(
        &self,
        names: &[String],
        tail_ceiling: usize,
        folded_languages: &[Language],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let exact: HashSet<&str> = names.iter().map(String::as_str).collect();
        // SQLite's `lower` folds ASCII letters only, as `to_ascii_lowercase`.
        let folded: HashSet<String> = names.iter().map(|name| name.to_ascii_lowercase()).collect();
        let rooted: HashSet<String> = names
            .iter()
            .map(|name| format!("\\{name}").to_ascii_lowercase())
            .collect();
        let php = Language::Php.as_str();
        let mut languages: Vec<&str> = folded_languages
            .iter()
            .map(|language| language.as_str())
            .chain([php])
            .collect();
        languages.sort_unstable();
        languages.dedup();
        let folded_languages: HashSet<&str> = folded_languages
            .iter()
            .map(|language| language.as_str())
            .collect();
        let sql = format!(
            "SELECT id, reference_name, reference_kind, language FROM unresolved_refs \
             WHERE instr(reference_name, '.') > 0 OR instr(reference_name, ':') > 0 \
             OR language IN ({}) ORDER BY id",
            std::iter::repeat_n("?", languages.len())
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut tails: HashMap<String, Vec<i64>> = HashMap::new();
        let mut chosen = BTreeSet::new();
        {
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(languages.iter()))?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let name = text_column(row, 1)?;
                let raw_kind = text_column(row, 2)?;
                if name.contains(['.', ':']) {
                    let tail = reference_name_tail_of(name, edge_kind_of(raw_kind)?);
                    if tail != name && exact.contains(tail) {
                        tails.entry(tail.to_string()).or_default().push(id);
                    }
                }
                let language = text_column(row, 3)?;
                let is_php = language == php;
                let in_any_case = (is_php || folded_languages.contains(language)) && {
                    let lowered = name.to_ascii_lowercase();
                    (is_php && rooted.contains(&lowered))
                        || (folded_languages.contains(language) && folded.contains(&lowered))
                };
                let through_class = is_php
                    && raw_kind == "calls"
                    && name.contains('\\')
                    && name.rsplit_once('.').is_some_and(|(class, member)| {
                        exact.contains(member)
                            || class
                                .rsplit('\\')
                                .next()
                                .is_some_and(|class| exact.contains(class))
                    });
                if in_any_case || through_class {
                    chosen.insert(id);
                }
            }
        }
        for ids in tails.values() {
            if ids.len() <= tail_ceiling {
                chosen.extend(ids.iter().copied());
            }
        }
        let mut by_id = self
            .conn
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id = ?1")?;
        chosen
            .into_iter()
            .map(|id| by_id.query_row([id], row_to_unresolved_ref))
            .collect()
    }

    /// The unresolved references written through one of `receivers`
    /// (`User.create`, `User::create`): a name that starts with the receiver
    /// and then `.` or `::`, each found by a range over `idx_unresolved_name`,
    /// however many there are (W5-07). Each row once, ordered by id.
    pub fn unresolved_refs_by_receivers(
        &self,
        receivers: &[String],
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut out: BTreeMap<i64, UnresolvedRef> = BTreeMap::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT * FROM unresolved_refs WHERE reference_name >= ?1 AND reference_name < ?2",
        )?;
        for receiver in receivers {
            // The byte after each separator bounds the range: `.` is 0x2E and
            // `/` 0x2F, `:` is 0x3A and `;` 0x3B.
            for (low, high) in [
                (format!("{receiver}."), format!("{receiver}/")),
                (format!("{receiver}::"), format!("{receiver}:;")),
            ] {
                for reference in stmt.query_map(params![low, high], row_to_unresolved_ref)? {
                    let reference = reference?;
                    if let Some(id) = reference.id {
                        out.entry(id).or_insert(reference);
                    }
                }
            }
        }
        Ok(out.into_values().collect())
    }

    /// The parked imports a sync that adds files should retry (upstream
    /// `getRetryableFailedImports`, #2392). An import names a file, a folder or
    /// a namespace, which the symbol-name lookup cannot match, so it is found by
    /// `path_keys` (each added file's [`import_path_keys`]) as its tail or its
    /// whole name, and by `names` (the namespaces and modules the changed files
    /// declare) as its whole name when its tail is something else. A
    /// reference written as a path is looked up as an import is. A key more
    /// than `per_key_ceiling` rows share is skipped, as the symbol lookup's
    /// keys are.
    pub fn retryable_failed_imports(
        &self,
        path_keys: &[String],
        names: &[String],
        per_key_ceiling: usize,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        if path_keys.is_empty() && names.is_empty() {
            return Ok(Vec::new());
        }
        let path_keys: HashSet<&str> = path_keys.iter().map(String::as_str).collect();
        let names: HashSet<&str> = names.iter().map(String::as_str).collect();
        // Each candidate row's id, name and kind first, read without copying,
        // the rest of a row only for the ones chosen (W5-09): the same rows in
        // the same order.
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, reference_name, reference_kind FROM unresolved_refs \
             WHERE reference_kind = 'imports' \
             OR (reference_kind = 'references' AND instr(reference_name, '/') > 0) ORDER BY id",
        )?;
        // One bucket per (lookup, key), counted against the ceiling apiece.
        let mut buckets: BTreeMap<(u8, String), Vec<i64>> = BTreeMap::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let name = text_column(row, 1)?;
            let tail = reference_name_tail_of(name, edge_kind_of(text_column(row, 2)?)?);
            if path_keys.contains(tail) {
                buckets.entry((0, tail.to_string())).or_default().push(id);
            }
            if path_keys.contains(name) {
                buckets.entry((1, name.to_string())).or_default().push(id);
            }
            if names.contains(name) && tail != name {
                buckets.entry((2, name.to_string())).or_default().push(id);
            }
        }
        drop(rows);
        let mut chosen = BTreeSet::new();
        for ids in buckets.values() {
            if ids.len() <= per_key_ceiling {
                chosen.extend(ids.iter().copied());
            }
        }
        let mut by_id = self
            .conn
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id = ?1")?;
        chosen
            .into_iter()
            .map(|id| by_id.query_row([id], row_to_unresolved_ref))
            .collect()
    }

    /// The JavaScript-family files whose import of `file` holds a binding that
    /// does not resolve into it (upstream #2450): another file X with an
    /// `imports` edge into `file`'s file node, X's `import` node around that
    /// edge's line, and within that node's lines either an unresolved
    /// `imports` reference other than the statement's own path (`W` of
    /// `import { Widget as W } from './widgets/Widget'` while the module
    /// declares no `Widget`) or an `imports` edge from X that import
    /// resolution did not make (`resolvedBy` other than `import`: `W` matched
    /// by its name, to a namesake in the module or elsewhere). The statement's
    /// edge to the file node itself is its path, not a binding. An edit that
    /// gives the module what such a binding imports reaches it by no name, as
    /// the binding is written under its own. Read it before `file`'s old
    /// nodes go: the edges into them go with them. Sorted, each file once.
    pub fn importers_with_unbound_bindings(&self, file: &str) -> rusqlite::Result<Vec<String>> {
        // CROSS JOIN pins the order: from the file's node to the edges into
        // it, never from every `imports` edge (`idx_edges_kind`).
        let mut stmt = self.conn.prepare_cached(
            r#"SELECT DISTINCT s.file_path
            FROM nodes f
            CROSS JOIN edges e ON e.target = f.id AND e.kind = 'imports'
            CROSS JOIN nodes s ON s.id = e.source
            CROSS JOIN nodes i ON i.file_path = s.file_path AND i.kind = 'import'
                AND i.start_line <= e.line AND e.line <= i.end_line
            WHERE f.file_path = ?1 AND f.kind = 'file' AND s.file_path <> ?1
                AND i.language IN
                    ('typescript', 'tsx', 'javascript', 'jsx', 'arkts', 'svelte', 'vue', 'astro')
                AND (
                    EXISTS (
                        SELECT 1 FROM unresolved_refs u
                        WHERE u.file_path = s.file_path AND u.reference_kind = 'imports'
                            AND u.line BETWEEN i.start_line AND i.end_line
                            AND u.reference_name <> i.name
                    )
                    OR EXISTS (
                        SELECT 1 FROM nodes x
                        CROSS JOIN edges b ON b.source = x.id AND b.kind = 'imports'
                        WHERE x.file_path = s.file_path
                            AND b.line BETWEEN i.start_line AND i.end_line
                            AND b.target <> f.id
                            AND (CASE WHEN json_valid(b.metadata)
                                THEN json_extract(b.metadata, '$.resolvedBy') END)
                                IS NOT 'import'
                    )
                )
            ORDER BY 1"#,
        )?;
        let rows = stmt.query_map([file], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Ports `getStats` count SQL from `upstream db/queries.ts:1746-1756`.
    pub fn counts(&self) -> rusqlite::Result<StoreCounts> {
        self.conn.query_row(
            r#"
            SELECT
              (SELECT COUNT(*) FROM nodes) AS node_count,
              (SELECT COUNT(*) FROM edges) AS edge_count,
              (SELECT COUNT(*) FROM files) AS file_count
            "#,
            [],
            |row| {
                Ok(StoreCounts {
                    node_count: row.get("node_count")?,
                    edge_count: row.get("edge_count")?,
                    file_count: row.get("file_count")?,
                })
            },
        )
    }

    /// Reclaim on-disk space after a full index by folding the WAL back into the
    /// main database file and truncating the `-wal` sidecar (`wal_checkpoint(TRUNCATE)`).
    /// This is a pure space-reclaim no-op on content: nodes, edges, FTS rows, and the
    /// `.schema` (including `sqlite_master` statement order) stay byte-identical, so it
    /// preserves golden byte-equivalence.
    ///
    /// `VACUUM` is intentionally NOT run here. VACUUM rewrites `sqlite_master`, which
    /// reorders the schema statements (all tables first, then indexes, then the FTS
    /// virtual table + triggers) relative to the upstream creation order. That reorder is
    /// content-preserving for data but breaks the order-sensitive Tier-1 `.schema`
    /// equivalence oracle (verified: even VACUUMing the upstream golden DB reorders its
    /// `.schema`). The checkpoint keeps the file compact while staying golden-equivalent.
    ///
    /// `PRAGMA incremental_vacuum` returns freelist pages to the OS without rewriting
    /// `sqlite_master`, so it shrinks fragmented auto_vacuum=INCREMENTAL DBs (the fresh
    /// DBs this port now creates) while preserving `.schema` order. On a legacy
    /// auto_vacuum=NONE DB it is a safe no-op, so old indexes are unaffected.
    ///
    /// Must be called with no active transaction.
    pub fn compact(&self) -> rusqlite::Result<()> {
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        // `PRAGMA incremental_vacuum` performs its work as its result is stepped; the
        // statement must be iterated to completion to reclaim ALL freelist pages.
        // rusqlite's `execute_batch` does not fully drive that iteration (it reclaims
        // only one page), so prepare + drain the rows explicitly.
        let mut stmt = self.conn.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Tune THIS connection for a from-scratch bulk index: drop `synchronous` to
    /// `OFF`, grow the page cache, stop memory-mapping the file, and (unless opted
    /// out) DEFER WAL auto-checkpointing for the whole run (#1231). This trades crash durability
    /// for throughput during the one-shot full index, which is always safe because
    /// the full-index path starts from an empty DB (`index --force` removes the file
    /// first) and is re-runnable from scratch on failure.
    ///
    /// WAL deferral (`wal_autocheckpoint=0`): SQLite's default 1000-page
    /// auto-checkpoint re-writes hot B-tree/FTS pages into the main DB file over and
    /// over during a bulk index — the dominant disk cost on HDD-class storage. With
    /// autocheckpoint off, the store becomes near-sequential WAL appends; WAL growth
    /// is bounded instead by [`Store::checkpoint_wal_if_over`], and the final
    /// [`Store::restore_default_pragmas`] / [`Store::compact`] folds the WAL back with
    /// one `wal_checkpoint(TRUNCATE)`. Set `CODEGRAPH_NO_WAL_DEFER=1` to keep the
    /// default autocheckpoint interval.
    ///
    /// No memory map (`mmap_size=0`): the page cache already holds the pages a
    /// full index writes and reads back, so mapping the file as well counted the
    /// database twice in peak RSS (once as cache, once as mapped file pages)
    /// without making the index faster.
    ///
    /// `synchronous`, `cache_size`, `mmap_size`, and `wal_autocheckpoint` are all
    /// durability/scheduling knobs only: none alters committed content, so golden
    /// byte-equivalence is preserved. `wal_autocheckpoint` is a per-connection
    /// setting, so a later reopen of this DB resets to the compiled default (1000).
    /// MUST be scoped to the CLI full-index connection — never the shared
    /// `Store::open` defaults used by sync/daemon/watch. Pair with
    /// [`Store::restore_default_pragmas`] (via a Drop guard) so the restore runs on
    /// both the happy and the error path.
    pub fn set_bulk_index_pragmas(&self) -> rusqlite::Result<()> {
        self.conn.pragma_update(None, "synchronous", "OFF")?;
        self.conn.pragma_update(None, "cache_size", -262_144)?;
        self.conn.pragma_update(None, "mmap_size", 0_i64)?;
        if !wal_defer_disabled() {
            self.conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        }
        Ok(())
    }

    /// Size in bytes of the `-wal` sidecar, or 0 when it does not exist (non-WAL
    /// mode, in-memory DB, or nothing written since the last checkpoint+reset).
    /// Drives the [`Store::checkpoint_wal_if_over`] valve (#1231).
    pub fn wal_size_bytes(&self) -> crate::connection::Result<u64> {
        Self::wal_size_bytes_for_path(self.path())
    }

    /// WAL-valve fold (#1231): when the `-wal` sidecar has grown past
    /// `threshold_bytes`, fold it back into the main DB with one
    /// `wal_checkpoint(TRUNCATE)` and report `true`; otherwise a cheap no-op
    /// returning `false`. Called between bulk-write batches while
    /// `wal_autocheckpoint=0` is in effect, so unbounded WAL growth (which fills
    /// the disk and makes every later read page through a multi-GB WAL) is capped
    /// without paying the default per-1000-page checkpoint churn. TRUNCATE is
    /// content-preserving — it only relocates already-committed frames — so index
    /// output stays byte-identical. Safe here because the bulk index holds the
    /// sole connection (no concurrent reader pins the WAL).
    ///
    /// Must be called with no active transaction.
    pub fn checkpoint_wal_if_over(&self, threshold_bytes: u64) -> crate::connection::Result<bool> {
        if self.wal_size_bytes()? <= threshold_bytes {
            return Ok(false);
        }
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|source| crate::connection::StoreError::CheckpointWal {
                path: self.path().to_path_buf(),
                source,
            })?;
        Ok(true)
    }

    /// Undo [`Store::set_bulk_index_pragmas`]: checkpoint the WAL back into the main
    /// file (truncating the `-wal` sidecar) and restore `synchronous=NORMAL` so any
    /// later reopen of this DB sees the shared default durability. The checkpoint
    /// leaves the on-disk file in the same compact, no-stray-`-wal` shape a NORMAL
    /// run produces.
    ///
    /// Must be called with no active transaction.
    pub fn restore_default_pragmas(&self) -> rusqlite::Result<()> {
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// Ports `getMetadata` from `upstream db/queries.ts:1798-1804`.
    pub fn get_project_metadata(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = ?",
                [key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Ports `setMetadata` from `upstream db/queries.ts:1806-1813`.
    pub fn set_project_metadata(&self, key: &str, value: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            "INSERT INTO project_metadata (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now_millis()],
        )
    }

    /// Delete a `project_metadata` row. Missing key is a no-op (returns 0).
    /// Complements [`Self::set_project_metadata`] for flags whose ABSENCE is the
    /// healthy state (see [`Self::clear_resolution_incomplete`]).
    pub fn delete_project_metadata(&self, key: &str) -> rusqlite::Result<usize> {
        self.conn
            .execute("DELETE FROM project_metadata WHERE key = ?", [key])
    }

    /// Every `project_metadata` row whose key starts with `prefix`, by key.
    pub fn project_metadata_with_prefix(
        &self,
        prefix: &str,
    ) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM project_metadata WHERE substr(key, 1, ?2) = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map(params![prefix, prefix.chars().count() as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    /// Delete every `project_metadata` row whose key starts with `prefix`.
    pub fn delete_project_metadata_with_prefix(&self, prefix: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM project_metadata WHERE substr(key, 1, ?2) = ?1",
            params![prefix, prefix.chars().count() as i64],
        )
    }

    /// `project_metadata` key for the incomplete-resolution MARKER (#1187).
    ///
    /// A full-index resolution pass SETS this at its start and DELETES it only on
    /// successful completion. A crash / Ctrl-C / #1122 watchdog kill mid-resolve
    /// therefore leaves the marker present, which is the signal that some
    /// `unresolved_refs` were never processed into edges (the interrupted-index
    /// "too-small blast radius" of the #1187 field report). This is deliberately
    /// distinct from "rows at rest": Rust RETAINS unresolvable rows as live retry
    /// state (#1240), so a bare row count is NOT a sound orphan signal here — the
    /// marker is.
    pub const RESOLUTION_INCOMPLETE_KEY: &'static str = "resolution_incomplete";

    /// Set the incomplete-resolution marker (#1187). Called at the START of a
    /// full-index resolution pass; cleared by [`Self::clear_resolution_incomplete`]
    /// only when that pass completes.
    pub fn set_resolution_incomplete(&self) -> rusqlite::Result<usize> {
        self.set_project_metadata(Self::RESOLUTION_INCOMPLETE_KEY, "1")
    }

    /// Clear the incomplete-resolution marker (#1187). Absence of the row is the
    /// healthy state, so this DELETEs rather than writing a sentinel — keeping the
    /// healthy `project_metadata` set identical to a pre-#1187 index.
    pub fn clear_resolution_incomplete(&self) -> rusqlite::Result<usize> {
        self.delete_project_metadata(Self::RESOLUTION_INCOMPLETE_KEY)
    }

    /// Whether the incomplete-resolution marker is set (#1187). `true` means a
    /// resolution pass was interrupted mid-run and the index is partial until the
    /// next `sync` sweeps it.
    pub fn is_resolution_incomplete(&self) -> rusqlite::Result<bool> {
        Ok(self
            .get_project_metadata(Self::RESOLUTION_INCOMPLETE_KEY)?
            .is_some())
    }

    /// `getStats` node-kind aggregation
    /// (`upstream db/queries.ts:1758-1763`):
    /// `SELECT kind, COUNT(*) FROM nodes GROUP BY kind`. Returned in SQLite's
    /// GROUP BY order (ascending by `kind` string), which `codegraph_status`
    /// renders directly (`tools.ts:2917-2923`).
    pub fn node_counts_by_kind(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) as count FROM nodes GROUP BY kind")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// `getStats` file-language aggregation
    /// (`upstream db/queries.ts:1773-1778`):
    /// `SELECT language, COUNT(*) FROM files GROUP BY language`.
    pub fn file_counts_by_language(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT language, COUNT(*) as count FROM files GROUP BY language")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// Ports `getDependentFilePaths` from
    /// `upstream db/queries.ts:1392-1402`: every file containing a
    /// symbol that has a cross-file edge (any kind except `contains`) INTO a
    /// symbol of `file_path`. Backs `getFileDependents`
    /// (`graph/queries.ts:134-143`) → the blast-radius / file-mode "used by"
    /// note. Returns DISTINCT source file paths in SQLite scan order.
    pub fn dependent_file_paths(&self, file_path: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT DISTINCT src.file_path AS fp
            FROM edges e
            JOIN nodes tgt ON tgt.id = e.target
            JOIN nodes src ON src.id = e.source
            WHERE tgt.file_path = ?1
              AND e.kind != 'contains'
              AND src.file_path != ?1"#,
        )?;
        let rows = stmt.query_map([file_path], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Source-site groups of every cross-file non-`contains` edge INTO a node in
    /// `file_path`, captured before that target file's node cascade. A NULL edge
    /// coordinate is returned as a whole-file fallback instead of being invented.
    pub fn reference_sites_dependent_on_file(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT src.file_path, e.source, e.line, e.col
            FROM edges e
            JOIN nodes tgt ON tgt.id = e.target
            JOIN nodes src ON src.id = e.source
            WHERE tgt.file_path = ?1
              AND e.kind != 'contains'
              AND src.file_path != ?1
            ORDER BY src.file_path, e.source, e.line, e.col"#,
        )?;
        let rows = stmt.query_map([file_path], row_to_file_reference_site)?;
        let mut out = BTreeSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out.into_iter().collect())
    }

    /// `(id, file_path, signature)` of every Python function and method, in
    /// id order. Sync reads the parameters a pytest fixture can feed.
    pub fn python_callable_signatures(&self) -> rusqlite::Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT id, file_path, COALESCE(signature, '')
            FROM nodes
            WHERE language = 'python' AND kind IN ('function', 'method')
            ORDER BY id"#,
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// The reference sites of everything `node_ids` reference: their
    /// non-`contains` edges and their unresolved rows, sorted and
    /// deduplicated.
    pub fn reference_sites_from_nodes(
        &self,
        node_ids: &[String],
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        let mut unique = node_ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let mut out = BTreeSet::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let params = chunk.iter().map(|p| p as &dyn ToSql).collect::<Vec<_>>();
            for sql in [
                format!(
                    r#"SELECT src.file_path, e.source, e.line, e.col
                    FROM edges e
                    JOIN nodes src ON src.id = e.source
                    WHERE e.kind != 'contains' AND e.source IN ({placeholders})"#
                ),
                format!(
                    r#"SELECT file_path, from_node_id, line, col
                    FROM unresolved_refs
                    WHERE from_node_id IN ({placeholders})"#
                ),
            ] {
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params.as_slice(), row_to_file_reference_site)?;
                for row in rows {
                    out.insert(row?);
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    /// The source sites of the function-value `references` edges
    /// (`{"fnRef": true}`) whose source lies in one of `file_paths`, sorted
    /// and deduplicated. Sync refreshes the ones that read through a Python
    /// module global whose writes changed.
    pub fn function_ref_sites_in_files(
        &self,
        file_paths: &[String],
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        let mut unique = file_paths.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let mut out = BTreeSet::new();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"SELECT src.file_path, e.source, e.line, e.col
                FROM edges e
                JOIN nodes src ON src.id = e.source
                WHERE e.kind = 'references'
                  AND json_extract(e.metadata, '$.fnRef') = 1
                  AND src.file_path IN ({placeholders})"#
            );
            let params = chunk.iter().map(|p| p as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_file_reference_site)?;
            for row in rows {
                out.insert(row?);
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Ports `getDependencyFilePaths` from
    /// `upstream db/queries.ts:1410-1420`: the OUTGOING twin of
    /// `dependent_file_paths` — every file containing a symbol that a symbol of
    /// `file_path` has a cross-file edge (any kind except `contains`) INTO.
    /// Backs `getFileDependencies` (`graph/queries.ts:118-124`) and the cycle
    /// detector's adjacency. Returns DISTINCT target file paths in SQLite scan
    /// order.
    pub fn dependency_file_paths(&self, file_path: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT DISTINCT tgt.file_path AS fp
            FROM edges e
            JOIN nodes src ON src.id = e.source
            JOIN nodes tgt ON tgt.id = e.target
            WHERE src.file_path = ?1
              AND e.kind != 'contains'
              AND tgt.file_path != ?1"#,
        )?;
        let rows = stmt.query_map([file_path], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// The Godot path-keyed reverse-dependency lane for `dependent_file_paths`:
    /// files carrying an `unresolved_refs` row whose normalized `reference_name`
    /// targets `file_path`, restricted to the Godot structural subkinds
    /// (`script_attach`/`scene_instance`/`ext_resource`/`group_member`/
    /// `signal_method`/`autoload`). Reuses the `resource_impact` (`audit --impact`)
    /// data source: a `.tscn`/`.tres`/`project.godot` owns no `file:` node so its
    /// refs never reach `edges`. A non-Godot ref has `reference_subkind = NULL` and
    /// is never returned, so other languages stay byte-unchanged; `gdscript_load_path`
    /// is excluded because those refs already resolve to real `edges`.
    /// Golden-neutral; returns DISTINCT referrers in scan order (caller sorts/dedups).
    pub fn dependent_file_paths_unresolved(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let target = strip_res_scheme(&file_path.replace('\\', "/")).to_string();
        let allow: [&str; 6] = [
            ReferenceSubkind::ScriptAttach.as_str(),
            ReferenceSubkind::SceneInstance.as_str(),
            ReferenceSubkind::ExtResource.as_str(),
            ReferenceSubkind::GroupMember.as_str(),
            ReferenceSubkind::SignalMethod.as_str(),
            ReferenceSubkind::Autoload.as_str(),
        ];
        let placeholders = std::iter::repeat_n("?", allow.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT file_path, reference_name FROM unresolved_refs \
             WHERE reference_subkind IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params = allow.iter().map(|s| s as &dyn ToSql).collect::<Vec<_>>();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (from_file, reference_name) = row?;
            if strip_res_scheme(&reference_name.replace('\\', "/")) == target {
                out.push(from_file);
            }
        }
        Ok(out)
    }

    /// The Godot `run/main_scene` reverse-dependency lane. The main-scene ref is
    /// emitted UNTAGGED (`reference_subkind = NULL`), so
    /// [`Self::dependent_file_paths_unresolved`]'s 6-subkind allow-list — which
    /// guards other-language byte-stability — deliberately excludes it. This
    /// dedicated lane surfaces it without loosening that allow-list: it matches
    /// only `unresolved_refs` rows whose OWNING file is a `project.godot`
    /// (basename gate) AND whose `reference_subkind IS NULL` AND whose
    /// `res://`-stripped, `/`-normalized `reference_name` equals `file_path`. The
    /// `project.godot` origin gate means a non-Godot None-subkind ref is never
    /// returned, so other languages stay byte-unchanged. Golden-neutral; returns
    /// DISTINCT referrers in scan order (caller sorts/dedups).
    pub fn dependent_file_paths_main_scene(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let target = strip_res_scheme(&file_path.replace('\\', "/")).to_string();
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT file_path, reference_name FROM unresolved_refs \
             WHERE reference_subkind IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (from_file, reference_name) = row?;
            if !is_project_godot_path(&from_file) {
                continue;
            }
            if strip_res_scheme(&reference_name.replace('\\', "/")) == target {
                out.push(from_file);
            }
        }
        Ok(out)
    }

    /// Godot loader-side reverse-dependency lane. A GDScript
    /// `preload/load("res://X")` RESOLVES to an `import:` node whose `name` is the
    /// `res://` path and whose `file_path` is the LOADER script. When `X` is a
    /// scene/resource it owns no `file:` node, so the resolved `imports` edge
    /// terminates on that `import:` node and `dependent_file_paths` (which keys on
    /// a target `file:` node) never surfaces the loader. This matches `import:`
    /// nodes whose `res://`-stripped, `/`-normalized name equals `file_path` and
    /// returns their owning loader paths. Only names carrying the `res://` scheme
    /// match, so non-Godot import nodes are never returned and other languages stay
    /// byte-unchanged. Golden-neutral; returns referrers in scan order (caller
    /// sorts/dedups).
    pub fn dependent_file_paths_via_import_name(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let target = file_path.replace('\\', "/");
        let mut out = Vec::new();
        for node in self.nodes_by_kind(NodeKind::Import)? {
            let Some(stripped) = node.name.strip_prefix("res://") else {
                continue;
            };
            if stripped.replace('\\', "/") == target {
                out.push(node.file_path);
            }
        }
        Ok(out)
    }

    /// Delete every resolution-produced edge whose SOURCE node lives in
    /// `file_path`. Extraction produces only `contains` edges and same-file
    /// value references (`references` with `{"valueRef": true}` and no
    /// coordinates); every other edge comes from reference resolution. Used
    /// only by incremental sync's correctness-first whole-file fallback;
    /// normal unchanged dependents use [`Self::delete_resolved_edges_at_sites`].
    /// Extraction edges are left intact because the file's nodes are not
    /// re-extracted.
    pub fn delete_resolved_edges_from_file(&self, file_path: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            &format!(
                r#"DELETE FROM edges
            WHERE id IN (
              SELECT e.id
              FROM edges e
              JOIN nodes src ON src.id = e.source
              WHERE src.file_path = ?1
                AND {RESOLUTION_EDGE}
            )"#
            ),
            [file_path],
        )
    }

    /// Delete every non-`contains` edge in the selected source-site groups while
    /// preserving extraction edges, other sites, and edges from other sources.
    /// The predicate deliberately excludes target, kind, and metadata because
    /// resolution can promote edge kinds and every sibling at a site is rebuilt.
    pub fn delete_resolved_edges_at_sites(
        &mut self,
        sites: &[ReferenceSite],
    ) -> rusqlite::Result<usize> {
        let unique = sites.iter().cloned().collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        {
            let mut stmt = tx.prepare_cached(
                r#"DELETE FROM edges
                WHERE source = ?1 AND line = ?2 AND col = ?3
                  AND kind != 'contains'"#,
            )?;
            for site in unique {
                removed += stmt.execute(params![site.from_node_id, site.line, site.col])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Atomically replace every resolution-produced edge and unresolved row for
    /// one source file with the freshly extracted references (#1833 / #1849).
    ///
    /// Incremental sync previously committed the two DELETE operations before
    /// attempting the INSERT. A disk/trigger/constraint failure during that
    /// insertion permanently removed the last committed relationship. Keeping
    /// all three operations in one `BEGIN IMMEDIATE` transaction makes the old
    /// graph answer survive any failed requeue.
    pub fn replace_resolution_state_from_file(
        &mut self,
        file_path: &str,
        refs: &[UnresolvedRef],
    ) -> rusqlite::Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = replace_file_resolution_in(&tx, file_path, refs)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Apply `replacements` in order in ONE `BEGIN IMMEDIATE` transaction:
    /// the statements of [`Self::replace_resolution_state_from_file`] and
    /// [`Self::replace_resolution_state_at_sites`], committed once for every
    /// dependent of a sync instead of once per file (W5-09a). A failure keeps
    /// every old answer, as each file's own transaction did for that file.
    pub fn replace_resolution_states(
        &mut self,
        replacements: &[ResolutionReplacement],
    ) -> rusqlite::Result<()> {
        if replacements.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for replacement in replacements {
            match replacement {
                ResolutionReplacement::File { file_path, refs } => {
                    replace_file_resolution_in(&tx, file_path, refs)?;
                }
                ResolutionReplacement::Sites { sites, refs } => {
                    replace_site_resolution_in(&tx, sites, refs)?;
                }
            }
        }
        tx.commit()
    }

    /// Atomically replace the resolution state of selected source-site sibling
    /// groups with freshly extracted references (#1833 / #1849).
    ///
    /// Target, edge kind, and reference name are intentionally absent from the
    /// predicates: resolution may promote an edge kind, and every sibling at one
    /// source coordinate must move together to preserve multiplicity.
    pub fn replace_resolution_state_at_sites(
        &mut self,
        sites: &[ReferenceSite],
        refs: &[UnresolvedRef],
    ) -> rusqlite::Result<usize> {
        if sites.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = replace_site_resolution_in(&tx, sites, refs)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Distinct source files of every resolution-produced edge whose TARGET node
    /// is named one of `names`. When a synced file changes the set of nodes
    /// sharing a name, the exact-name resolution of refs that already resolved to
    /// that name (in any file, including the referencing file itself) can change
    /// confidence or pick a different target, but those edges survive untouched.
    /// This path-level graph query remains available to existing callers; selective
    /// sync uses [`Self::reference_sites_of_edges_to_named_targets`] so unrelated
    /// refs in the same file retain row identity. `contains` edges are excluded;
    /// results are in SQLite scan order per chunk.
    pub fn source_files_of_edges_to_named_targets(
        &self,
        names: &[String],
    ) -> rusqlite::Result<Vec<String>> {
        let mut out = Vec::new();
        if names.is_empty() {
            return Ok(out);
        }
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"SELECT DISTINCT src.file_path AS fp
                FROM edges e
                JOIN nodes tgt ON tgt.id = e.target
                JOIN nodes src ON src.id = e.source
                WHERE tgt.name IN ({placeholders})
                  AND e.kind != 'contains'"#
            );
            let params = chunk.iter().map(|n| n as &dyn ToSql).collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
            for path in rows {
                out.push(path?);
            }
        }
        Ok(out)
    }

    /// The `calls` edges whose source is a node of `file_path` that have a
    /// site, with the name of the node each reaches (W4-DART-03b: a sync tells
    /// the chain links among a file's calls by the member each names). The
    /// `CROSS JOIN` keeps the file's nodes the outer loop: left to itself the
    /// planner walks every `calls` edge of the project through the kind index.
    pub fn call_sites_in_file(
        &self,
        file_path: &str,
    ) -> rusqlite::Result<Vec<(String, i64, i64, String)>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT e.source, e.line, e.col, tgt.name
            FROM nodes src
            CROSS JOIN edges e ON e.source = src.id
            JOIN nodes tgt ON tgt.id = e.target
            WHERE src.file_path = ?1 AND e.kind = 'calls'
              AND e.line IS NOT NULL AND e.col IS NOT NULL
            ORDER BY e.source, e.line, e.col, tgt.name"#,
        )?;
        let rows = stmt.query_map([file_path], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Precise source-site counterpart of
    /// [`Self::source_files_of_edges_to_named_targets`]. Returns surviving
    /// cross-file non-`contains` edges whose target name is selected. Multiple
    /// matching sibling edges collapse to one site group; NULL coordinates expose
    /// a whole-file fallback rather than silently skipping that source file.
    pub fn reference_sites_of_edges_to_named_targets(
        &self,
        names: &[String],
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        self.reference_sites_to_named_targets(names, "!=")
    }

    /// Same-file counterpart of
    /// [`Self::reference_sites_of_edges_to_named_targets`]: surviving
    /// non-`contains` edges whose source and target share a file and whose
    /// target name is selected. An exact-name resolution's confidence counts every
    /// node of that name in the project, so a competitor arriving in or leaving
    /// another file changes an edge inside a file nobody touched. Sync selects
    /// only names whose candidates changed, not names whose nodes merely moved: a
    /// candidate outside the referencing file is scored without its position, and
    /// the same-file target outranks it either way.
    pub fn reference_sites_of_same_file_edges_to_named_targets(
        &self,
        names: &[String],
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        self.reference_sites_to_named_targets(names, "=")
    }

    /// The body of both named-target site queries; `file_relation` is the SQL
    /// operator relating the edge's source and target files, `!=` or `=`.
    fn reference_sites_to_named_targets(
        &self,
        names: &[String],
        file_relation: &str,
    ) -> rusqlite::Result<Vec<FileReferenceSite>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_names = names.to_vec();
        unique_names.sort_unstable();
        unique_names.dedup();
        let mut out = BTreeSet::new();
        for chunk in unique_names.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"SELECT src.file_path, e.source, e.line, e.col
                FROM edges e
                JOIN nodes tgt ON tgt.id = e.target
                JOIN nodes src ON src.id = e.source
                WHERE tgt.name IN ({placeholders})
                  AND e.kind != 'contains'
                  AND src.file_path {file_relation} tgt.file_path
                ORDER BY src.file_path, e.source, e.line, e.col"#
            );
            let params = chunk
                .iter()
                .map(|name| name as &dyn ToSql)
                .collect::<Vec<_>>();
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), row_to_file_reference_site)?;
            for row in rows {
                out.insert(row?);
            }
        }
        Ok(out.into_iter().collect())
    }
}

/// SQL predicate over edge alias `e`: an edge reference resolution produced.
/// Extraction produces `contains` edges and same-file value references (a
/// `references` edge with `{"valueRef": true}` and no coordinates), which a
/// re-resolution of an unchanged file must keep.
const RESOLUTION_EDGE: &str = "e.kind != 'contains' AND NOT (e.kind = 'references' AND e.line IS NULL AND CASE WHEN json_valid(e.metadata) THEN json_extract(e.metadata, '$.valueRef') END = 1)";

fn row_to_file_reference_site(row: &Row<'_>) -> rusqlite::Result<FileReferenceSite> {
    let file_path = row.get(0)?;
    let from_node_id = row.get(1)?;
    let line = row.get::<_, Option<i64>>(2)?;
    let col = row.get::<_, Option<i64>>(3)?;
    let site = match (line, col) {
        (Some(line), Some(col)) => Some(ReferenceSite {
            from_node_id,
            line,
            col,
        }),
        _ => None,
    };
    Ok(FileReferenceSite { file_path, site })
}

fn query_nodes<P>(conn: &Connection, sql: &str, params: P) -> rusqlite::Result<Vec<Node>>
where
    P: rusqlite::Params,
{
    // A statement prepared once per connection, and each field's column
    // found once per statement rather than by name on every row (W5-08):
    // the same values [`row_to_node`] reads.
    let mut stmt = conn.prepare_cached(sql)?;
    let columns = NodeColumns::of(&stmt)?;
    let rows = stmt.query_map(params, |row| columns.node(row))?;
    rows.collect()
}

/// The column each [`Node`] field is read from in one statement's rows.
struct NodeColumns {
    id: usize,
    kind: usize,
    name: usize,
    qualified_name: usize,
    file_path: usize,
    language: usize,
    start_line: usize,
    end_line: usize,
    start_column: usize,
    end_column: usize,
    docstring: usize,
    signature: usize,
    visibility: usize,
    is_exported: usize,
    is_async: usize,
    is_static: usize,
    is_abstract: usize,
    decorators: usize,
    type_parameters: usize,
    return_type: usize,
    updated_at: usize,
}

impl NodeColumns {
    fn of(stmt: &rusqlite::Statement<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: stmt.column_index("id")?,
            kind: stmt.column_index("kind")?,
            name: stmt.column_index("name")?,
            qualified_name: stmt.column_index("qualified_name")?,
            file_path: stmt.column_index("file_path")?,
            language: stmt.column_index("language")?,
            start_line: stmt.column_index("start_line")?,
            end_line: stmt.column_index("end_line")?,
            start_column: stmt.column_index("start_column")?,
            end_column: stmt.column_index("end_column")?,
            docstring: stmt.column_index("docstring")?,
            signature: stmt.column_index("signature")?,
            visibility: stmt.column_index("visibility")?,
            is_exported: stmt.column_index("is_exported")?,
            is_async: stmt.column_index("is_async")?,
            is_static: stmt.column_index("is_static")?,
            is_abstract: stmt.column_index("is_abstract")?,
            decorators: stmt.column_index("decorators")?,
            type_parameters: stmt.column_index("type_parameters")?,
            return_type: stmt.column_index("return_type")?,
            updated_at: stmt.column_index("updated_at")?,
        })
    }

    /// [`row_to_node`] of `row`, by the columns found.
    fn node(&self, row: &Row<'_>) -> rusqlite::Result<Node> {
        Ok(Node {
            id: row.get(self.id)?,
            kind: parse_node_kind(row.get::<_, String>(self.kind)?)?,
            name: row.get(self.name)?,
            qualified_name: row.get(self.qualified_name)?,
            file_path: row.get(self.file_path)?,
            language: parse_language(row.get::<_, String>(self.language)?)?,
            start_line: row.get(self.start_line)?,
            end_line: row.get(self.end_line)?,
            start_column: row.get(self.start_column)?,
            end_column: row.get(self.end_column)?,
            docstring: row.get(self.docstring)?,
            signature: row.get(self.signature)?,
            visibility: row.get(self.visibility)?,
            is_exported: row.get::<_, i64>(self.is_exported)? == 1,
            is_async: row.get::<_, i64>(self.is_async)? == 1,
            is_static: row.get::<_, i64>(self.is_static)? == 1,
            is_abstract: row.get::<_, i64>(self.is_abstract)? == 1,
            decorators: parse_json_array(row.get(self.decorators)?)?,
            type_parameters: parse_json_array(row.get(self.type_parameters)?)?,
            return_type: row.get(self.return_type)?,
            updated_at: row.get(self.updated_at)?,
        })
    }
}

fn query_edges<P>(conn: &Connection, sql: &str, params: P) -> rusqlite::Result<Vec<Edge>>
where
    P: rusqlite::Params,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_edge)?;
    rows.collect()
}

pub(crate) fn row_to_node(row: &Row<'_>) -> rusqlite::Result<Node> {
    Ok(Node {
        id: row.get("id")?,
        kind: parse_node_kind(row.get::<_, String>("kind")?)?,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        file_path: row.get("file_path")?,
        language: parse_language(row.get::<_, String>("language")?)?,
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        start_column: row.get("start_column")?,
        end_column: row.get("end_column")?,
        docstring: row.get("docstring")?,
        signature: row.get("signature")?,
        visibility: row.get("visibility")?,
        is_exported: row.get::<_, i64>("is_exported")? == 1,
        is_async: row.get::<_, i64>("is_async")? == 1,
        is_static: row.get::<_, i64>("is_static")? == 1,
        is_abstract: row.get::<_, i64>("is_abstract")? == 1,
        decorators: parse_json_array(row.get("decorators")?)?,
        type_parameters: parse_json_array(row.get("type_parameters")?)?,
        return_type: row.get("return_type")?,
        updated_at: row.get("updated_at")?,
    })
}

pub(crate) fn row_to_edge(row: &Row<'_>) -> rusqlite::Result<Edge> {
    Ok(Edge {
        id: row.get("id")?,
        source: row.get("source")?,
        target: row.get("target")?,
        kind: parse_edge_kind(row.get::<_, String>("kind")?)?,
        metadata: parse_json_value(row.get("metadata")?)?,
        line: row.get("line")?,
        col: row.get("col")?,
        provenance: row.get("provenance")?,
    })
}

pub(crate) fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        path: row.get("path")?,
        content_hash: row.get("content_hash")?,
        language: parse_language(row.get::<_, String>("language")?)?,
        size: row.get("size")?,
        modified_at: millis_column(row, "modified_at")?,
        indexed_at: millis_column(row, "indexed_at")?,
        node_count: row.get("node_count")?,
        errors: parse_json_array(row.get("errors")?)?,
        generated: row.get::<_, i64>("generated")? != 0,
    })
}

/// Read a millisecond timestamp column tolerating either INTEGER or REAL
/// storage. The upstream writes `Date.now()` (a JS number) into `files.modified_at`,
/// which SQLite stores as REAL with sub-millisecond fraction
/// (`upstream db/queries.ts` upserts; see golden mini DB where
/// `typeof(modified_at) = real`). A plain `i64` get fails on those rows, so
/// floor a REAL to its integer millisecond value.
fn millis_column(row: &Row<'_>, name: &str) -> rusqlite::Result<i64> {
    use rusqlite::types::ValueRef;
    match row.get_ref(name)? {
        ValueRef::Integer(i) => Ok(i),
        ValueRef::Real(f) => Ok(f as i64),
        ValueRef::Null => Ok(0),
        other => Err(rusqlite::Error::InvalidColumnType(
            row.as_ref().column_index(name).unwrap_or(0),
            name.to_string(),
            other.data_type(),
        )),
    }
}

/// The tail a parked reference waits under (upstream `referenceNameTail`,
/// #1240, #2392): the part a new symbol's plain name could match, after the
/// last `.` or `:` (`util.greet` -> `greet`, `mod::fn/2` -> `fn`, an Erlang
/// arity dropped). An import, or a reference written as a path, names a FILE,
/// so its tail is the path's last segment cut at its first dot
/// (`package:app/b.dart` -> `b`, `./foo` -> `foo`, `inc/db.php` -> `db`): the
/// stem [`import_path_keys`] takes from a file that could satisfy it.
pub fn reference_name_tail(reference_name: &str, reference_kind: EdgeKind) -> String {
    reference_name_tail_of(reference_name, reference_kind).to_string()
}

/// [`reference_name_tail`] as the part of the name it is.
fn reference_name_tail_of(reference_name: &str, reference_kind: EdgeKind) -> &str {
    // A reference written as a path, but not an Erlang `fun mod:f/2` value.
    let path_reference = reference_kind == EdgeKind::References
        && reference_name.contains('/')
        && strip_erlang_arity(reference_name) == reference_name;
    if reference_kind == EdgeKind::Imports || path_reference {
        let path = reference_name.trim_end_matches('/');
        if let Some(slash) = path.rfind('/') {
            let stem = file_name_stem(&path[slash + 1..]);
            // `..` and `.` name a folder only through the importing file.
            if !stem.chars().all(|c| c == '.') {
                return stem;
            }
        }
    }
    let base = strip_erlang_arity(reference_name);
    match base.rfind(['.', ':']) {
        Some(idx) => &base[idx + 1..],
        None => base,
    }
}

/// `name` without a trailing `/<1-3 digits>` arity (`f/1` -> `f`).
fn strip_erlang_arity(name: &str) -> &str {
    match name.rsplit_once('/') {
        Some((base, arity))
            if !base.is_empty()
                && (1..=3).contains(&arity.len())
                && arity.chars().all(|c| c.is_ascii_digit()) =>
        {
            base
        }
        _ => name,
    }
}

/// A file or folder name up to its first dot past the first character:
/// `b.dart` -> `b`, `types.d.ts` -> `types`, `.eslintrc.js` -> `.eslintrc`.
fn file_name_stem(name: &str) -> &str {
    match name.get(1..).and_then(|rest| rest.find('.')) {
        Some(dot) => &name[..dot + 1],
        None => name,
    }
}

/// What an import of the file at `file_path` can be written as (upstream
/// `importPathKeys`, #2392): its name (`b.h`, a bare include) and stem (`b`,
/// the tail of `a/b.h`, `./b`, `pkg.b`), and the same two for its folder,
/// which an import names for its index file (`./ui` -> `ui/index.ts`, `pkg`
/// -> `pkg/__init__.py`) or as a Go package.
pub fn import_path_keys(file_path: &str) -> Vec<String> {
    let parts: Vec<&str> = file_path.split('/').collect();
    let mut keys: Vec<String> = Vec::new();
    for name in &parts[parts.len().saturating_sub(2)..] {
        for key in [*name, file_name_stem(name)] {
            if !key.is_empty() && !keys.iter().any(|existing| existing == key) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

/// A text column of `row`, borrowed rather than copied.
fn text_column<'row>(row: &'row Row<'_>, index: usize) -> rusqlite::Result<&'row str> {
    let value = row.get_ref(index)?;
    value
        .as_str()
        .map_err(|_| rusqlite::Error::InvalidColumnType(index, String::new(), value.data_type()))
}

/// The edge kind a stored `reference_kind` reads as, a function reference's
/// as `references` ([`row_to_unresolved_ref`]).
fn edge_kind_of(raw_kind: &str) -> rusqlite::Result<EdgeKind> {
    if raw_kind == "function_ref" {
        return Ok(EdgeKind::References);
    }
    edge_kind_named(raw_kind).ok_or_else(|| enum_error(raw_kind.to_string(), "edge kind"))
}

/// The columns `unresolved_refs` and `resolved_refs` share, in table order.
const UNRESOLVED_REF_COLUMNS: &str = "id, from_node_id, reference_name, reference_kind, line, col, \
     candidates, file_path, language, reference_subkind";

pub(crate) fn row_to_unresolved_ref(row: &Row<'_>) -> rusqlite::Result<UnresolvedRef> {
    let raw_kind = row.get::<_, String>("reference_kind")?;
    let is_function_ref = raw_kind == "function_ref";
    let reference_kind = if is_function_ref {
        EdgeKind::References
    } else {
        parse_edge_kind(raw_kind)?
    };
    Ok(UnresolvedRef {
        id: row.get("id")?,
        from_node_id: row.get("from_node_id")?,
        reference_name: row.get("reference_name")?,
        reference_kind,
        line: row.get("line")?,
        col: row.get("col")?,
        candidates: parse_optional_json_array(row.get("candidates")?)?,
        file_path: row.get("file_path")?,
        language: parse_language(row.get::<_, String>("language")?)?,
        is_function_ref,
        reference_subkind: parse_reference_subkind(row.get("reference_subkind")?)?,
    })
}

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn json_array_or_null(values: &[String]) -> rusqlite::Result<Option<String>> {
    if values.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(values)
            .map(Some)
            .map_err(json_to_sql_error)
    }
}

fn parse_json_array(raw: Option<String>) -> rusqlite::Result<Vec<String>> {
    parse_optional_json_array(raw).map(Option::unwrap_or_default)
}

fn parse_optional_json_array(raw: Option<String>) -> rusqlite::Result<Option<Vec<String>>> {
    raw.map(|text| serde_json::from_str(&text).map_err(json_to_sql_error))
        .transpose()
}

fn parse_json_value(raw: Option<String>) -> rusqlite::Result<Option<Value>> {
    raw.map(|text| serde_json::from_str(&text).map_err(json_to_sql_error))
        .transpose()
}

fn json_to_sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn enum_error(value: String, ty: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(format!("unknown {ty}: {value}"))
}

fn parse_node_kind(value: String) -> rusqlite::Result<NodeKind> {
    let kind = match value.as_str() {
        "file" => NodeKind::File,
        "module" => NodeKind::Module,
        "class" => NodeKind::Class,
        "struct" => NodeKind::Struct,
        "interface" => NodeKind::Interface,
        "trait" => NodeKind::Trait,
        "protocol" => NodeKind::Protocol,
        "function" => NodeKind::Function,
        "method" => NodeKind::Method,
        "property" => NodeKind::Property,
        "field" => NodeKind::Field,
        "variable" => NodeKind::Variable,
        "constant" => NodeKind::Constant,
        "enum" => NodeKind::Enum,
        "enum_member" => NodeKind::EnumMember,
        "type_alias" => NodeKind::TypeAlias,
        "namespace" => NodeKind::Namespace,
        "parameter" => NodeKind::Parameter,
        "import" => NodeKind::Import,
        "export" => NodeKind::Export,
        "route" => NodeKind::Route,
        "component" => NodeKind::Component,
        "union" => NodeKind::Union,
        _ => return Err(enum_error(value, "node kind")),
    };
    Ok(kind)
}

fn parse_edge_kind(value: String) -> rusqlite::Result<EdgeKind> {
    edge_kind_named(&value).ok_or_else(|| enum_error(value, "edge kind"))
}

/// The edge kind a stored name is, if any.
fn edge_kind_named(value: &str) -> Option<EdgeKind> {
    let kind = match value {
        "contains" => EdgeKind::Contains,
        "calls" => EdgeKind::Calls,
        "imports" => EdgeKind::Imports,
        "exports" => EdgeKind::Exports,
        "extends" => EdgeKind::Extends,
        "implements" => EdgeKind::Implements,
        "references" => EdgeKind::References,
        "type_of" => EdgeKind::TypeOf,
        "returns" => EdgeKind::Returns,
        "instantiates" => EdgeKind::Instantiates,
        "overrides" => EdgeKind::Overrides,
        "decorates" => EdgeKind::Decorates,
        "navigates" => EdgeKind::Navigates,
        _ => return None,
    };
    Some(kind)
}

fn parse_reference_subkind(value: Option<String>) -> rusqlite::Result<Option<ReferenceSubkind>> {
    let Some(text) = value else {
        return Ok(None);
    };
    let subkind = match text.as_str() {
        "script_attach" => ReferenceSubkind::ScriptAttach,
        "scene_instance" => ReferenceSubkind::SceneInstance,
        "ext_resource" => ReferenceSubkind::ExtResource,
        "group_member" => ReferenceSubkind::GroupMember,
        "signal_method" => ReferenceSubkind::SignalMethod,
        "gdscript_load_path" => ReferenceSubkind::GdscriptLoadPath,
        "autoload" => ReferenceSubkind::Autoload,
        _ => return Err(enum_error(text, "reference subkind")),
    };
    Ok(Some(subkind))
}

fn strip_res_scheme(s: &str) -> &str {
    s.strip_prefix("res://").unwrap_or(s)
}

fn is_project_godot_path(path: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base == "project.godot")
}

fn parse_language(value: String) -> rusqlite::Result<Language> {
    let language = match value.as_str() {
        "typescript" => Language::TypeScript,
        "javascript" => Language::JavaScript,
        "tsx" => Language::Tsx,
        "jsx" => Language::Jsx,
        "arkts" => Language::ArkTs,
        "python" => Language::Python,
        "go" => Language::Go,
        "rust" => Language::Rust,
        "java" => Language::Java,
        "c" => Language::C,
        "cpp" => Language::Cpp,
        "csharp" => Language::CSharp,
        "razor" => Language::Razor,
        "php" => Language::Php,
        "ruby" => Language::Ruby,
        "swift" => Language::Swift,
        "kotlin" => Language::Kotlin,
        "dart" => Language::Dart,
        "svelte" => Language::Svelte,
        "vue" => Language::Vue,
        "astro" => Language::Astro,
        "liquid" => Language::Liquid,
        "pascal" => Language::Pascal,
        "scala" => Language::Scala,
        "lua" => Language::Lua,
        "luau" => Language::Luau,
        "objc" => Language::ObjC,
        "r" => Language::R,
        "solidity" => Language::Solidity,
        "nix" => Language::Nix,
        "terraform" => Language::Terraform,
        "erlang" => Language::Erlang,
        "cfml" => Language::Cfml,
        "yaml" => Language::Yaml,
        "twig" => Language::Twig,
        "xml" => Language::Xml,
        "properties" => Language::Properties,
        "gdscript" => Language::Gdscript,
        "godot_scene" => Language::GodotScene,
        "godot_resource" => Language::GodotResource,
        "godot_project" => Language::GodotProject,
        "unknown" => Language::Unknown,
        _ => return Err(enum_error(value, "language")),
    };
    Ok(language)
}

fn validate_nodes(nodes: &[Node]) -> rusqlite::Result<()> {
    for node in nodes {
        if node.id.is_empty()
            || node.name.is_empty()
            || node.file_path.is_empty()
            || node.qualified_name.is_empty()
        {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "node has missing required fields: {}",
                node.id
            )));
        }
    }
    Ok(())
}

fn existing_node_ids(
    conn: &Connection,
    ids: &[&str],
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let mut unique = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    for chunk in unique.chunks(SQLITE_PARAM_CHUNK_SIZE) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id FROM nodes WHERE id IN ({placeholders})");
        let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        for id in rows {
            out.insert(id?);
        }
    }
    Ok(out)
}

/// One dependent's resolution state to replace in
/// [`Store::replace_resolution_states`].
#[derive(Debug, Clone)]
pub enum ResolutionReplacement {
    /// Every resolution edge and stored reference of the file, by these
    /// references ([`Store::replace_resolution_state_from_file`]).
    File {
        file_path: String,
        refs: Vec<UnresolvedRef>,
    },
    /// The site groups, by these references
    /// ([`Store::replace_resolution_state_at_sites`]).
    Sites {
        sites: Vec<ReferenceSite>,
        refs: Vec<UnresolvedRef>,
    },
}

/// The body of [`Store::replace_resolution_state_from_file`], in `tx`.
fn replace_file_resolution_in(
    tx: &Connection,
    file_path: &str,
    refs: &[UnresolvedRef],
) -> rusqlite::Result<usize> {
    let removed = tx.execute(
        &format!(
            r#"DELETE FROM edges
            WHERE id IN (
              SELECT e.id
              FROM edges e
              JOIN nodes src ON src.id = e.source
              WHERE src.file_path = ?1
                AND {RESOLUTION_EDGE}
            )"#
        ),
        [file_path],
    )?;
    tx.execute(
        "DELETE FROM unresolved_refs WHERE file_path = ?1",
        [file_path],
    )?;
    tx.execute(
        "DELETE FROM resolved_refs WHERE file_path = ?1",
        [file_path],
    )?;
    insert_unresolved_refs_in_connection(tx, refs)?;
    Ok(removed)
}

/// The body of [`Store::replace_resolution_state_at_sites`], in `tx`.
fn replace_site_resolution_in(
    tx: &Connection,
    sites: &[ReferenceSite],
    refs: &[UnresolvedRef],
) -> rusqlite::Result<usize> {
    let unique = sites.iter().cloned().collect::<BTreeSet<_>>();
    if unique.is_empty() {
        return Ok(0);
    }
    let mut removed = 0;
    {
        let mut delete_edges = tx.prepare_cached(
            r#"DELETE FROM edges
                WHERE source = ?1 AND line = ?2 AND col = ?3
                  AND kind != 'contains'"#,
        )?;
        let mut delete_refs = tx.prepare_cached(
            r#"DELETE FROM unresolved_refs
                WHERE from_node_id = ?1 AND line = ?2 AND col = ?3"#,
        )?;
        let mut delete_resolved = tx.prepare_cached(
            r#"DELETE FROM resolved_refs
                WHERE from_node_id = ?1 AND line = ?2 AND col = ?3"#,
        )?;
        for site in unique {
            removed +=
                delete_edges.execute(params![site.from_node_id.as_str(), site.line, site.col])?;
            delete_refs.execute(params![site.from_node_id.as_str(), site.line, site.col])?;
            delete_resolved.execute(params![site.from_node_id.as_str(), site.line, site.col])?;
        }
    }
    insert_unresolved_refs_in_connection(tx, refs)?;
    Ok(removed)
}

/// Insert unresolved references through `conn`, which may be either the Store's
/// connection or a caller-owned transaction. Keeping the statement body here is
/// what lets incremental sync compose edge deletion, stale-ref deletion, and
/// fresh-ref insertion into one crash-safe transaction.
fn insert_unresolved_refs_in_connection(
    conn: &Connection,
    refs: &[UnresolvedRef],
) -> rusqlite::Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let from_ids = refs
        .iter()
        .map(|unresolved| unresolved.from_node_id.as_str())
        .collect::<Vec<_>>();
    let existing_node_ids = existing_node_ids(conn, &from_ids)?;
    let mut stmt = conn.prepare_cached(
        r#"
        INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language, reference_subkind)
        VALUES (@fromNodeId, @referenceName, @referenceKind, @line, @col, @candidates, @filePath, @language, @referenceSubkind)
        "#,
    )?;
    for unresolved in refs {
        if !existing_node_ids.contains(&unresolved.from_node_id) {
            continue;
        }
        let candidates = unresolved
            .candidates
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_to_sql_error)?;
        let reference_kind = if unresolved.is_function_ref {
            "function_ref"
        } else {
            unresolved.reference_kind.as_str()
        };
        stmt.execute(named_params! {
            "@fromNodeId": unresolved.from_node_id,
            "@referenceName": unresolved.reference_name,
            "@referenceKind": reference_kind,
            "@line": unresolved.line,
            "@col": unresolved.col,
            "@candidates": candidates,
            "@filePath": unresolved.file_path,
            "@language": unresolved.language.as_str(),
            "@referenceSubkind": unresolved.reference_subkind.map(|s| s.as_str()),
        })?;
    }
    Ok(())
}

fn fts_query(query: &str) -> String {
    query
        .replace("::", " ")
        .replace(['\'', '"', '*', '(', ')', ':', '^'], "")
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .filter(|term| {
            !matches!(
                term.to_ascii_uppercase().as_str(),
                "AND" | "OR" | "NOT" | "NEAR"
            )
        })
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn append_kind_language_filters(
    sql: &mut String,
    params: &mut Vec<Box<dyn ToSql>>,
    kind_column: &str,
    language_column: &str,
    kinds: &[NodeKind],
    languages: &[Language],
) {
    if !kinds.is_empty() {
        let placeholders = vec!["?"; kinds.len()].join(",");
        sql.push_str(&format!(" AND {kind_column} IN ({placeholders})"));
        for kind in kinds {
            params.push(Box::new(kind.as_str().to_string()));
        }
    }
    if !languages.is_empty() {
        let placeholders = vec!["?"; languages.len()].join(",");
        sql.push_str(&format!(" AND {language_column} IN ({placeholders})"));
        for language in languages {
            params.push(Box::new(language.as_str().to_string()));
        }
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_millis()
        .try_into()
        .expect("current epoch milliseconds must fit in i64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path(test_name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "codegraph-store-{test_name}-{}-{}.db",
            std::process::id(),
            now_millis()
        ));
        path
    }

    fn store(test_name: &str) -> Store {
        Store::open(&temp_db_path(test_name)).expect("open temp store")
    }

    #[test]
    fn project_metadata_prefix_reads_and_deletes_only_its_rows() {
        let store = store("metadata-prefix");
        for (key, value) in [
            ("surface:src/b.ts", "2"),
            ("surface:src/a.ts", "1"),
            ("surfaces", "other"),
            ("indexed_with_version", "0.1.0"),
        ] {
            store.set_project_metadata(key, value).unwrap();
        }
        assert_eq!(
            store.project_metadata_with_prefix("surface:").unwrap(),
            [
                ("surface:src/a.ts".to_string(), "1".to_string()),
                ("surface:src/b.ts".to_string(), "2".to_string()),
            ]
        );
        assert_eq!(
            store
                .delete_project_metadata_with_prefix("surface:")
                .unwrap(),
            2
        );
        assert!(
            store
                .project_metadata_with_prefix("surface:")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.get_project_metadata("surfaces").unwrap().as_deref(),
            Some("other")
        );
        assert_eq!(
            store
                .get_project_metadata("indexed_with_version")
                .unwrap()
                .as_deref(),
            Some("0.1.0")
        );
    }

    fn node(id: &str, name: &str, file_path: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: format!("{file_path}::{name}"),
            file_path: file_path.to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 3,
            start_column: 0,
            end_column: 1,
            docstring: Some(format!("doc for {name}")),
            signature: Some(format!("fn {name}()")),
            visibility: Some("public".to_string()),
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: vec!["test_attr".to_string()],
            type_parameters: vec!["T".to_string()],
            return_type: Some("usize".to_string()),
            updated_at: 1,
        }
    }

    fn file(path: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: "hash".to_string(),
            language: Language::Rust,
            size: 42,
            modified_at: 1,
            indexed_at: 2,
            node_count: 1,
            errors: vec!["warn".to_string()],
            generated: false,
        }
    }

    #[test]
    fn upsert_file_round_trips_the_generated_flag() {
        let store = store("generated-round-trip");

        let mut rec = file("payroll.go");
        rec.generated = true;
        store.upsert_file(&rec).unwrap();
        assert!(store.file_by_path("payroll.go").unwrap().unwrap().generated);

        // Re-upserting the SAME path with a false verdict must overwrite it. This
        // half is what catches a missing `ON CONFLICT ... DO UPDATE SET generated`:
        // without it the row keeps its stale `1` forever.
        rec.generated = false;
        store.upsert_file(&rec).unwrap();
        assert!(!store.file_by_path("payroll.go").unwrap().unwrap().generated);

        // And back again, so the update is not one-directional.
        rec.generated = true;
        store.upsert_file(&rec).unwrap();
        assert!(store.file_by_path("payroll.go").unwrap().unwrap().generated);
    }

    #[test]
    fn generated_paths_among_returns_only_flagged_paths() {
        let store = store("generated-paths-among");

        let mut flagged = file("api.pb.go");
        flagged.generated = true;
        store.upsert_file(&flagged).unwrap();
        store.upsert_file(&file("handwritten.go")).unwrap();

        let asked = vec![
            "api.pb.go".to_string(),
            "handwritten.go".to_string(),
            // A path with NO row at all is simply absent, never an error.
            "never_indexed.go".to_string(),
        ];
        let got = store.generated_paths_among(&asked).unwrap();
        assert_eq!(got, HashSet::from(["api.pb.go".to_string()]));

        // An EMPTY slice returns an empty set. MEASURED: `chunks()` on an empty
        // slice yields ZERO chunks, so the loop body never runs and no `IN ()`
        // syntax error is reachable — the early return saves an allocation and a
        // sort, it is not what makes this correct. Asserted anyway: the contract
        // is "no rows", and a future rewrite that builds SQL before chunking
        // would break it.
        assert!(store.generated_paths_among(&[]).unwrap().is_empty());
    }

    #[test]
    fn generated_paths_among_crosses_the_parameter_chunk_boundary() {
        let store = store("generated-paths-chunked");

        // More paths than SQLITE_PARAM_CHUNK_SIZE, with flagged rows placed in the
        // FIRST and LAST chunk, so a loop that only ever runs once still fails.
        let total = SQLITE_PARAM_CHUNK_SIZE * 2 + 7;
        let mut asked = Vec::with_capacity(total);
        for i in 0..total {
            let path = format!("gen/f{i:04}.go");
            let mut rec = file(&path);
            rec.generated = i == 0 || i == total - 1;
            store.upsert_file(&rec).unwrap();
            asked.push(path);
        }

        let got = store.generated_paths_among(&asked).unwrap();
        assert_eq!(
            got,
            HashSet::from([
                "gen/f0000.go".to_string(),
                format!("gen/f{:04}.go", total - 1),
            ])
        );
    }

    #[test]
    fn round_trip_insert_then_read_by_id_name_and_file() {
        let mut store = store("round-trip");
        store.upsert_file(&file("src/lib.rs")).unwrap();
        let inserted = node("function:one", "calculateTotal", "src/lib.rs");

        store.upsert_nodes(std::slice::from_ref(&inserted)).unwrap();

        assert_eq!(
            store.node_by_id("function:one").unwrap(),
            Some(inserted.clone())
        );
        assert_eq!(
            store.nodes_by_name("calculateTotal").unwrap(),
            vec![inserted.clone()]
        );
        assert_eq!(
            store.nodes_by_file_path("src/lib.rs").unwrap(),
            vec![inserted]
        );
        assert_eq!(
            store.file_by_path("src/lib.rs").unwrap(),
            Some(file("src/lib.rs"))
        );
    }

    #[test]
    fn union_node_round_trips_kind_through_sqlite() {
        // `parse_node_kind` is a string match with a rejecting `_` arm, so a new
        // NodeKind that extraction can WRITE but this cannot READ fails only at
        // runtime, with `unknown node kind: union`. The extractor never round-trips
        // through SQLite, so no extraction test can catch that.
        let mut store = store("union-round-trip");
        store.upsert_file(&file("src/bits.rs")).unwrap();
        let mut inserted = node("union:bits", "Bits", "src/bits.rs");
        inserted.kind = NodeKind::Union;

        store.upsert_nodes(std::slice::from_ref(&inserted)).unwrap();

        let read_back = store.node_by_id("union:bits").unwrap();
        assert_eq!(read_back, Some(inserted.clone()));
        assert_eq!(read_back.unwrap().kind, NodeKind::Union);
    }

    #[test]
    fn gdscript_node_round_trips_language_through_sqlite() {
        let mut store = store("gdscript-round-trip");
        store.upsert_file(&file("scripts/player.gd")).unwrap();
        let mut inserted = node("function:gd", "ready", "scripts/player.gd");
        inserted.language = Language::Gdscript;

        store.upsert_nodes(std::slice::from_ref(&inserted)).unwrap();

        let read_back = store.node_by_id("function:gd").unwrap();
        assert_eq!(read_back, Some(inserted.clone()));
        assert_eq!(read_back.unwrap().language, Language::Gdscript);
    }

    #[test]
    fn batch_1000_nodes_commits_and_mid_batch_validation_error_rolls_back() {
        let mut batch_store = store("batch");
        let nodes = (0..1000)
            .map(|i| node(&format!("function:{i}"), &format!("f{i}"), "src/batch.rs"))
            .collect::<Vec<_>>();
        batch_store.upsert_nodes(&nodes).unwrap();
        assert_eq!(batch_store.counts().unwrap().node_count, 1000);

        let mut rollback_store = store("batch-rollback");
        let mut poisoned = nodes;
        poisoned[500].name.clear();
        assert!(rollback_store.upsert_nodes(&poisoned).is_err());
        assert_eq!(rollback_store.counts().unwrap().node_count, 0);
    }

    #[test]
    fn cascade_delete_node_removes_edges() {
        let mut store = store("cascade");
        let source = node("function:source", "source", "src/a.rs");
        let target = node("function:target", "target", "src/a.rs");
        store.upsert_nodes(&[source, target]).unwrap();
        store
            .insert_edges(&[Edge {
                id: None,
                source: "function:source".to_string(),
                target: "function:target".to_string(),
                kind: EdgeKind::Calls,
                metadata: Some(serde_json::json!({"kind":"direct"})),
                line: Some(2),
                col: Some(4),
                provenance: Some("tree-sitter".to_string()),
            }])
            .unwrap();
        assert_eq!(store.counts().unwrap().edge_count, 1);

        store.delete_node("function:target").unwrap();

        assert_eq!(store.counts().unwrap().edge_count, 0);
    }

    #[test]
    fn fts_sync_insert_update_delete_tracks_name_docstring_and_signature() {
        let mut store = store("fts");
        let mut n = node("function:search", "AlphaSearch", "src/search.rs");
        n.docstring = Some("needle documentation".to_string());
        n.signature = Some("fn alpha_signature()".to_string());
        store.upsert_nodes(&[n.clone()]).unwrap();

        assert_eq!(
            store.search_nodes_fts("Alpha", 10, 0).unwrap()[0].node.id,
            n.id
        );
        assert_eq!(
            store.search_nodes_fts("needle", 10, 0).unwrap()[0].node.id,
            n.id
        );
        assert_eq!(
            store.search_nodes_fts("alpha_signature", 10, 0).unwrap()[0]
                .node
                .id,
            n.id
        );

        n.name = "BetaSearch".to_string();
        n.qualified_name = "src/search.rs::BetaSearch".to_string();
        n.docstring = Some("haystack documentation".to_string());
        n.signature = Some("fn beta_signature()".to_string());
        store.upsert_nodes(&[n.clone()]).unwrap();

        assert!(store.search_nodes_fts("Alpha", 10, 0).unwrap().is_empty());
        assert_eq!(
            store.search_nodes_fts("Beta", 10, 0).unwrap()[0].node.id,
            n.id
        );

        store.delete_node(&n.id).unwrap();
        assert!(store.search_nodes_fts("Beta", 10, 0).unwrap().is_empty());
    }

    #[test]
    fn case_insensitive_name_lookup_uses_lower_name_shape() {
        let mut store = store("case");
        let n = node("function:case", "CamelCase", "src/case.rs");
        store.upsert_nodes(std::slice::from_ref(&n)).unwrap();

        assert_eq!(store.nodes_by_lower_name("camelcase").unwrap(), vec![n]);
    }

    #[test]
    fn edges_by_source_kind_filters_outgoing_edges() {
        let mut store = store("edges");
        let source = node("function:source", "source", "src/e.rs");
        let target = node("function:target", "target", "src/e.rs");
        let other = node("function:other", "other", "src/e.rs");
        store.upsert_nodes(&[source, target, other]).unwrap();
        store
            .insert_edges(&[
                Edge {
                    id: None,
                    source: "function:source".to_string(),
                    target: "function:target".to_string(),
                    kind: EdgeKind::Calls,
                    metadata: None,
                    line: None,
                    col: None,
                    provenance: None,
                },
                Edge {
                    id: None,
                    source: "function:source".to_string(),
                    target: "function:other".to_string(),
                    kind: EdgeKind::References,
                    metadata: None,
                    line: None,
                    col: None,
                    provenance: None,
                },
            ])
            .unwrap();

        let calls = store
            .edges_by_source_kind("function:source", Some(EdgeKind::Calls))
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target, "function:target");
    }

    #[test]
    fn edges_by_kind_returns_only_requested_kind_in_row_order() {
        let mut store = store("edges-by-kind");
        let source = node("function:source", "source", "src/e.rs");
        let first = node("function:first", "first", "src/e.rs");
        let second = node("function:second", "second", "src/e.rs");
        let third = node("function:third", "third", "src/e.rs");
        store.upsert_nodes(&[source, first, second, third]).unwrap();
        store
            .insert_edges(&[
                Edge {
                    id: None,
                    source: "function:source".to_string(),
                    target: "function:first".to_string(),
                    kind: EdgeKind::Extends,
                    metadata: None,
                    line: None,
                    col: None,
                    provenance: None,
                },
                Edge {
                    id: None,
                    source: "function:source".to_string(),
                    target: "function:second".to_string(),
                    kind: EdgeKind::Implements,
                    metadata: None,
                    line: None,
                    col: None,
                    provenance: None,
                },
                Edge {
                    id: None,
                    source: "function:source".to_string(),
                    target: "function:third".to_string(),
                    kind: EdgeKind::Extends,
                    metadata: None,
                    line: None,
                    col: None,
                    provenance: None,
                },
            ])
            .unwrap();

        let extends = store.edges_by_kind(EdgeKind::Extends).unwrap();
        assert_eq!(
            extends
                .iter()
                .map(|edge| edge.target.as_str())
                .collect::<Vec<_>>(),
            vec!["function:first", "function:third"]
        );
        let implements = store.edges_by_kind(EdgeKind::Implements).unwrap();
        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].target, "function:second");
    }

    #[test]
    fn unresolved_refs_and_metadata_round_trip() {
        let mut store = store("unresolved");
        let n = node("function:source", "source", "src/u.rs");
        store.upsert_nodes(&[n]).unwrap();
        let unresolved = UnresolvedRef {
            id: None,
            from_node_id: "function:source".to_string(),
            reference_name: "Missing".to_string(),
            reference_kind: EdgeKind::References,
            line: 9,
            col: 2,
            candidates: Some(vec!["candidate".to_string()]),
            file_path: "src/u.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(std::slice::from_ref(&unresolved))
            .unwrap();

        assert_eq!(store.all_unresolved_refs().unwrap().len(), 1);
        let by_file = store.unresolved_refs_by_file_path("src/u.rs").unwrap();
        assert_eq!(by_file[0].reference_name, unresolved.reference_name);
        let extends = UnresolvedRef {
            reference_name: "Base".to_string(),
            reference_kind: EdgeKind::Extends,
            ..unresolved.clone()
        };
        store
            .insert_unresolved_refs(std::slice::from_ref(&extends))
            .unwrap();
        let of_kind = store.unresolved_refs_of_kind(EdgeKind::Extends).unwrap();
        assert_eq!(
            of_kind
                .iter()
                .map(|reference| reference.reference_name.as_str())
                .collect::<Vec<_>>(),
            ["Base"]
        );
        assert_eq!(
            store
                .unresolved_refs_of_kind(EdgeKind::References)
                .unwrap()
                .len(),
            1
        );

        assert_eq!(store.get_project_metadata("root").unwrap(), None);
        store.set_project_metadata("root", "/tmp/project").unwrap();
        store.set_project_metadata("root", "/tmp/project2").unwrap();
        assert_eq!(
            store.get_project_metadata("root").unwrap(),
            Some("/tmp/project2".to_string())
        );
    }

    #[test]
    fn concurrent_endpoint_delete_does_not_abort_insert_edges() {
        // Given: a db with two nodes and a second connection that deletes the
        // edge's target, mimicking the daemon catch-up sync racing the watcher.
        let path = temp_db_path("concurrent-edge-fk");
        let mut store = Store::open(&path).unwrap();
        store.upsert_file(&file("a.rs")).unwrap();
        store
            .upsert_nodes(&[
                node("function:src", "src", "a.rs"),
                node("function:dst", "dst", "a.rs"),
            ])
            .unwrap();

        let deleter = Store::open(&path).unwrap();
        deleter
            .connection()
            .execute("DELETE FROM nodes WHERE id = 'function:dst'", [])
            .unwrap();

        // When: inserting an edge whose target the other connection just removed.
        let edge = Edge {
            id: None,
            source: "function:src".to_string(),
            target: "function:dst".to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: Some(1),
            col: Some(0),
            provenance: None,
        };
        let result = store.insert_edges(std::slice::from_ref(&edge));

        // Then: the sync survives — the now-absent endpoint is dropped, not raised
        // as `FOREIGN KEY constraint failed`.
        assert!(
            result.is_ok(),
            "insert_edges aborted on a concurrently-deleted endpoint: {:?}",
            result.err()
        );
        assert!(store.all_edges().unwrap().is_empty());
    }

    #[test]
    fn concurrent_source_delete_does_not_abort_insert_unresolved_refs() {
        // Given: a db with a node and a second connection that deletes it.
        let path = temp_db_path("concurrent-ref-fk");
        let mut store = Store::open(&path).unwrap();
        store.upsert_file(&file("a.rs")).unwrap();
        store
            .upsert_nodes(&[node("function:src", "src", "a.rs")])
            .unwrap();

        let deleter = Store::open(&path).unwrap();
        deleter
            .connection()
            .execute("DELETE FROM nodes WHERE id = 'function:src'", [])
            .unwrap();

        // When: inserting a ref whose source the other connection just removed.
        let unresolved = UnresolvedRef {
            id: None,
            from_node_id: "function:src".to_string(),
            reference_name: "gone".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            col: 0,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        let result = store.insert_unresolved_refs(std::slice::from_ref(&unresolved));

        // Then: the orphan ref is dropped, the sync is not aborted by an FK error.
        assert!(
            result.is_ok(),
            "insert_unresolved_refs aborted on a concurrently-deleted source: {:?}",
            result.err()
        );
        assert!(store.all_unresolved_refs().unwrap().is_empty());
    }

    #[test]
    fn concurrent_writers_never_abort_insert_edges_with_stale_snapshot() {
        // Given: two connections to one db racing the exact daemon-catch-up vs
        // watcher-sync pattern — one repeatedly deletes and re-inserts the edge's
        // target while the other repeatedly inserts an edge to it. Pre-fix the
        // endpoint snapshot was read on an autocommit connection, so a delete
        // committed between that read and the insert tripped `FOREIGN KEY
        // constraint failed`; `BEGIN IMMEDIATE` serialises the writers so the
        // snapshot and inserts see one consistent view.
        let path = temp_db_path("concurrent-stale-snapshot");
        {
            let mut seed = Store::open(&path).unwrap();
            seed.upsert_file(&file("a.rs")).unwrap();
            seed.upsert_nodes(&[node("function:src", "src", "a.rs")])
                .unwrap();
        }

        let churn_path = path.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let churn_stop = std::sync::Arc::clone(&stop);
        let churn = std::thread::spawn(move || {
            let mut churner = Store::open(&churn_path).unwrap();
            let dst = node("function:dst", "dst", "a.rs");
            while !churn_stop.load(std::sync::atomic::Ordering::Relaxed) {
                // Lock contention against the writer's IMMEDIATE transaction is
                // expected; a real concurrent writer just retries, so ignore it.
                let _ = churner.upsert_nodes(std::slice::from_ref(&dst));
                let _ = churner
                    .connection()
                    .execute("DELETE FROM nodes WHERE id = 'function:dst'", []);
            }
        });

        // When: the other writer inserts an edge to the churned endpoint many times.
        let mut writer = Store::open(&path).unwrap();
        let edge = Edge {
            id: None,
            source: "function:src".to_string(),
            target: "function:dst".to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: Some(1),
            col: Some(0),
            provenance: None,
        };
        let mut aborted: Option<String> = None;
        for _ in 0..2_000 {
            if let Err(err) = writer.insert_edges(std::slice::from_ref(&edge)) {
                aborted = Some(err.to_string());
                break;
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        churn.join().unwrap();

        // Then: no iteration ever aborted with a foreign-key error.
        assert!(
            aborted.is_none(),
            "insert_edges aborted under a concurrent endpoint deleter: {aborted:?}"
        );
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    /// Every edge kind, `navigates` included, reads back as the kind written:
    /// the parser and `EdgeKind::ALL` stay in step.
    #[test]
    fn every_edge_kind_reads_back_as_written() {
        let mut store = store("edge-kinds");
        let a = node("function:a", "alpha", "src/a.rs");
        let b = node("function:b", "beta", "src/b.rs");
        store.upsert_nodes(&[a, b]).unwrap();
        let written: Vec<Edge> = EdgeKind::ALL
            .into_iter()
            .map(|kind| edge("function:a", "function:b", kind))
            .collect();
        store.insert_edges(&written).unwrap();
        let mut read: Vec<&'static str> = store
            .all_edges()
            .unwrap()
            .into_iter()
            .map(|edge| edge.kind.as_str())
            .collect();
        read.sort_unstable();
        let mut expected = EdgeKind::ALL.map(EdgeKind::as_str).to_vec();
        expected.sort_unstable();
        assert_eq!(read, expected);
    }

    #[test]
    fn node_lookups_by_kind_qualified_name_and_ids() {
        let mut store = store("node-lookups");
        let a = node("function:a", "alpha", "src/a.rs");
        let mut b = node("class:b", "Beta", "src/b.rs");
        b.kind = NodeKind::Class;
        store.upsert_nodes(&[a.clone(), b.clone()]).unwrap();

        assert_eq!(
            store.nodes_by_kind(NodeKind::Function).unwrap(),
            vec![a.clone()]
        );
        assert_eq!(
            store.nodes_by_kind(NodeKind::Class).unwrap(),
            vec![b.clone()]
        );
        assert!(store.nodes_by_kind(NodeKind::Enum).unwrap().is_empty());

        assert_eq!(
            store.nodes_by_qualified_name("src/a.rs::alpha").unwrap(),
            vec![a.clone()]
        );
        assert!(store.nodes_by_qualified_name("missing").unwrap().is_empty());

        assert!(store.nodes_by_ids(&[]).unwrap().is_empty());
        let map = store
            .nodes_by_ids(&[
                "function:a".to_string(),
                "class:b".to_string(),
                "function:a".to_string(),
                "does:not-exist".to_string(),
            ])
            .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("function:a"), Some(&a));
        assert_eq!(map.get("class:b"), Some(&b));

        assert_eq!(store.node_count_by_file_path("src/a.rs").unwrap(), 1);
        assert_eq!(store.node_count_by_file_path("nope").unwrap(), 0);
    }

    #[test]
    fn all_node_names_returns_distinct_names() {
        let mut store = store("all-names");
        store
            .upsert_nodes(&[
                node("function:a", "shared", "src/a.rs"),
                node("function:b", "shared", "src/b.rs"),
                node("function:c", "unique", "src/c.rs"),
            ])
            .unwrap();
        let mut names = store.all_node_names().unwrap();
        names.sort();
        assert_eq!(names, vec!["shared".to_string(), "unique".to_string()]);
    }

    #[test]
    fn seedable_infix_includes_variables_constants_and_folds_separators() {
        let mut store = store("callable-infix");
        let mut cls = node("class:holder", "UserProfileIdHolder", "src/holder.rs");
        cls.kind = NodeKind::Class;
        let mut var = node("variable:v", "userProfileId", "src/var.rs");
        var.kind = NodeKind::Variable;
        let mut constant = node("constant:c", "USER_PROFILE_ID", "src/constant.rs");
        constant.kind = NodeKind::Constant;
        store
            .upsert_nodes(&[
                node("function:long", "updateUserProfileIdMapping", "src/b.rs"),
                node("function:short", "userProfileId2", "src/a.rs"),
                node("function:snake", "read_user_profile_id", "src/c.rs"),
                node("function:other", "unrelated", "src/d.rs"),
                cls,
                var,
                constant,
            ])
            .unwrap();

        let hits = store
            .seedable_nodes_by_name_infix("userprofileid", 50)
            .unwrap();
        let names: Vec<&str> = hits.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "userProfileId",
                "userProfileId2",
                "USER_PROFILE_ID",
                "read_user_profile_id",
                "updateUserProfileIdMapping"
            ],
            "seedable kinds only, shortest name first; the separator-stripped form matches snake_case"
        );

        assert!(
            store
                .seedable_nodes_by_name_infix("userprofileid", 1)
                .unwrap()
                .len()
                == 1,
            "the limit is honoured"
        );
        assert!(
            store
                .seedable_nodes_by_name_infix("nosuchneedle", 50)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn distinct_non_file_node_names_omits_file_and_import() {
        let mut store = store("distinct-non-file");
        store.upsert_file(&file("src/svc.rs")).unwrap();
        let mut file_node = node("file:src/svc.rs", "svc.rs", "src/svc.rs");
        file_node.kind = NodeKind::File;
        let mut import_node = node("import:react", "React", "src/svc.rs");
        import_node.kind = NodeKind::Import;
        let svc = node("function:svc", "CheckoutService", "src/svc.rs");
        store.upsert_nodes(&[file_node, import_node, svc]).unwrap();

        let names = store.distinct_non_file_node_names().unwrap();
        let just_names: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(just_names, vec!["CheckoutService".to_string()]);
        assert_eq!(names[0].1, NodeKind::Function);
    }

    #[test]
    fn search_variants_filter_by_kind_and_language() {
        let mut store = store("search-variants");
        let mut func = node("function:f", "SearchTarget", "src/f.rs");
        func.docstring = Some("primary target".to_string());
        let mut class = node("class:c", "SearchTarget", "src/c.py");
        class.kind = NodeKind::Class;
        class.language = Language::Python;
        store.upsert_nodes(&[func.clone(), class.clone()]).unwrap();

        let filtered = store
            .search_nodes_fts_filtered("SearchTarget", &[NodeKind::Function], &[], 10, 0)
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].node.id, "function:f");

        let by_lang = store
            .search_nodes_fts_filtered("SearchTarget", &[], &[Language::Python], 10, 0)
            .unwrap();
        assert_eq!(by_lang.len(), 1);
        assert_eq!(by_lang[0].node.id, "class:c");

        assert!(
            store
                .search_nodes_fts_filtered("", &[], &[], 10, 0)
                .unwrap()
                .is_empty()
        );
        assert!(store.search_nodes_fts("   ", 10, 0).unwrap().is_empty());

        let like = store.search_nodes_like("Search", &[], &[], 10, 0).unwrap();
        assert_eq!(like.len(), 2);
        assert!(like.iter().all(|r| r.score > 0.0));

        let all = store
            .search_all_by_filters(&[NodeKind::Function], &[], 10)
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].score, 1.0);
        assert_eq!(all[0].node.id, "function:f");
    }

    #[test]
    fn exact_name_lookups_nocase_and_filtered() {
        let mut store = store("exact-name");
        store
            .upsert_nodes(&[
                node("function:x", "Widget", "src/x.rs"),
                node("function:y", "Widget", "src/y.rs"),
            ])
            .unwrap();

        let nocase = store
            .nodes_by_exact_name_nocase("widget", &[], &[])
            .unwrap();
        assert_eq!(nocase.len(), 2);

        let filtered = store
            .nodes_by_exact_name_filtered("Widget", &[NodeKind::Function], &[Language::Rust])
            .unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(
            store
                .nodes_by_exact_name_filtered("missing", &[], &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exact_name_queries_seek_lower_name_index_for_hits_misses_and_filters() {
        let mut store = store("exact-name-plan");
        store
            .upsert_nodes(&[
                node("function:x", "HandleRequest", "src/x.rs"),
                node("function:y", "handlerequest", "src/y.rs"),
            ])
            .unwrap();

        let ids = store
            .nodes_by_lower_name("HANDLEREQUEST")
            .unwrap()
            .into_iter()
            .map(|node| node.id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2, "caller casing must not affect lookup");

        fn plan(store: &Store, sql: &str, params: &[&dyn ToSql]) -> String {
            let explained = format!("EXPLAIN QUERY PLAN {sql}");
            let mut stmt = store.conn.prepare(&explained).unwrap();
            let details = stmt
                .query_map(params, |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            details.join(" | ")
        }

        let miss = plan(
            &store,
            "SELECT * FROM nodes WHERE lower(name) = lower(?) LIMIT 20",
            &[&"not-a-symbol"],
        );
        assert!(
            miss.contains("SEARCH nodes") && miss.contains("idx_nodes_lower_name"),
            "miss regressed to a scan: {miss}"
        );

        let filtered = plan(
            &store,
            "SELECT * FROM nodes WHERE lower(name) = lower(?) \
             AND kind IN (?) AND language IN (?) LIMIT 20",
            &[&"HandleRequest", &"function", &"rust"],
        );
        assert!(
            filtered.contains("SEARCH nodes") && filtered.contains("idx_nodes_lower_name"),
            "filtered lookup regressed to a scan: {filtered}"
        );
    }

    #[test]
    fn parse_language_accepts_nix() {
        assert_eq!(parse_language("nix".to_string()).unwrap(), Language::Nix);
    }

    #[test]
    fn parse_language_accepts_terraform() {
        assert_eq!(
            parse_language("terraform".to_string()).unwrap(),
            Language::Terraform
        );
    }

    #[test]
    fn parse_language_accepts_erlang() {
        assert_eq!(
            parse_language("erlang".to_string()).unwrap(),
            Language::Erlang
        );
    }

    #[test]
    fn parse_language_accepts_cfml() {
        assert_eq!(parse_language("cfml".to_string()).unwrap(), Language::Cfml);
    }

    #[test]
    fn edges_by_target_and_all_getters() {
        let mut store = store("edges-target");
        store
            .upsert_nodes(&[
                node("function:src", "src", "e.rs"),
                node("function:dst", "dst", "e.rs"),
            ])
            .unwrap();
        store
            .insert_edges(&[
                edge("function:src", "function:dst", EdgeKind::Calls),
                edge("function:src", "function:dst", EdgeKind::References),
            ])
            .unwrap();

        let incoming = store
            .edges_by_target_kind("function:dst", Some(EdgeKind::Calls))
            .unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].kind, EdgeKind::Calls);
        let all_incoming = store.edges_by_target_kind("function:dst", None).unwrap();
        assert_eq!(all_incoming.len(), 2);
        let all_outgoing = store.edges_by_source_kind("function:src", None).unwrap();
        assert_eq!(all_outgoing.len(), 2);

        assert_eq!(store.all_nodes().unwrap().len(), 2);
        assert_eq!(store.all_edges().unwrap().len(), 2);
    }

    #[test]
    fn insert_edges_empty_is_noop() {
        let mut store = store("edges-empty");
        store.insert_edges(&[]).unwrap();
        assert_eq!(store.all_edges().unwrap().len(), 0);
    }

    #[test]
    fn insert_edges_dedups_identical_edge_identity() {
        let mut store = store("edges-dedup");
        store
            .upsert_nodes(&[
                node("function:src", "src", "e.rs"),
                node("function:dst", "dst", "e.rs"),
            ])
            .unwrap();

        let mut e = edge("function:src", "function:dst", EdgeKind::Calls);
        e.line = Some(5);
        e.col = Some(3);
        store.insert_edges(std::slice::from_ref(&e)).unwrap();
        store.insert_edges(std::slice::from_ref(&e)).unwrap();

        assert_eq!(
            store.all_edges().unwrap().len(),
            1,
            "same edge identity inserted twice must collapse to one row"
        );
    }

    #[test]
    fn insert_edges_dedups_null_coordinate_edge_identity() {
        let mut store = store("edges-dedup-null");
        store
            .upsert_nodes(&[
                node("file:e.rs", "e.rs", "e.rs"),
                node("function:dst", "dst", "e.rs"),
            ])
            .unwrap();

        let e = edge("file:e.rs", "function:dst", EdgeKind::Contains);
        store.insert_edges(std::slice::from_ref(&e)).unwrap();
        store.insert_edges(std::slice::from_ref(&e)).unwrap();

        assert_eq!(
            store.all_edges().unwrap().len(),
            1,
            "coordinate-less edge (NULL line/col) identity must dedup via IFNULL folding"
        );
    }

    #[test]
    fn insert_edges_keeps_distinct_identities_apart() {
        let mut store = store("edges-distinct");
        store
            .upsert_nodes(&[
                node("function:src", "src", "e.rs"),
                node("function:dst", "dst", "e.rs"),
            ])
            .unwrap();

        let mut a = edge("function:src", "function:dst", EdgeKind::Calls);
        a.line = Some(5);
        a.col = Some(3);
        let mut b = edge("function:src", "function:dst", EdgeKind::Calls);
        b.line = Some(6);
        b.col = Some(3);
        let c = edge("function:src", "function:dst", EdgeKind::References);
        store.insert_edges(&[a, b, c]).unwrap();

        assert_eq!(
            store.all_edges().unwrap().len(),
            3,
            "differing line, or differing kind, are distinct edge identities"
        );
    }

    #[test]
    fn file_records_delete_and_aggregations() {
        let mut store = store("files-agg");
        store.upsert_file(&file("src/a.rs")).unwrap();
        let mut py = file("src/b.py");
        py.language = Language::Python;
        store.upsert_file(&py).unwrap();
        store
            .upsert_nodes(&[node("function:a", "a", "src/a.rs")])
            .unwrap();

        let files = store.all_files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.rs");

        let by_lang = store.file_counts_by_language().unwrap();
        assert!(by_lang.contains(&("rust".to_string(), 1)));
        assert!(by_lang.contains(&("python".to_string(), 1)));

        let by_kind = store.node_counts_by_kind().unwrap();
        assert_eq!(by_kind, vec![("function".to_string(), 1)]);

        store.delete_file_record("src/a.rs").unwrap();
        assert_eq!(store.file_by_path("src/a.rs").unwrap(), None);
        assert_eq!(store.node_count_by_file_path("src/a.rs").unwrap(), 0);
        assert_eq!(store.all_files().unwrap().len(), 1);
    }

    #[test]
    fn unresolved_ref_batch_and_multi_key_reads() {
        let mut store = store("unresolved-batch");
        store
            .upsert_nodes(&[
                node("function:src1", "src1", "a.rs"),
                node("function:src2", "src2", "b.rs"),
            ])
            .unwrap();
        let mk = |from: &str, name: &str, file: &str, lang: Language| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[
                mk("function:src1", "Target", "a.rs", Language::Rust),
                mk("function:src2", "Other", "b.rs", Language::Rust),
            ])
            .unwrap();

        assert_eq!(store.unresolved_refs_count().unwrap(), 2);

        let first = store.unresolved_refs_batch(0, 1).unwrap();
        assert_eq!(first.len(), 1);
        let after = store
            .unresolved_refs_batch(first[0].id.unwrap(), 10)
            .unwrap();
        assert_eq!(after.len(), 1);

        assert!(store.unresolved_refs_by_files(&[]).unwrap().is_empty());
        assert!(store.unresolved_refs_by_names(&[]).unwrap().is_empty());
        let by_files = store
            .unresolved_refs_by_files(&["a.rs".to_string(), "b.rs".to_string(), "a.rs".to_string()])
            .unwrap();
        assert_eq!(by_files.len(), 2);
        let by_names = store
            .unresolved_refs_by_names(&["Target".to_string()])
            .unwrap();
        assert_eq!(by_names.len(), 1);
        assert_eq!(by_names[0].reference_name, "Target");
    }

    #[test]
    fn edge_sites_are_found_by_folded_target_names_in_both_file_relations() {
        let mut store = store("folded-targets");
        let php = |id: &str, name: &str, file: &str| {
            let mut n = node(id, name, file);
            n.language = Language::Php;
            n
        };
        let mut js = node("function:js", "caller", "c.js");
        js.language = Language::JavaScript;
        store
            .upsert_nodes(&[
                php("function:render", "render", "view.php"),
                php("function:FormatPrice", "FormatPrice", "helpers.php"),
                php("function:local", "Local", "view.php"),
                js.clone(),
            ])
            .unwrap();
        let edge = |source: &str, target: &str, line: i64| Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: Some(line),
            col: Some(0),
            provenance: None,
        };
        store
            .insert_edges(&[
                edge("function:render", "function:FormatPrice", 2),
                edge("function:render", "function:local", 3),
                edge("function:js", "function:FormatPrice", 4),
            ])
            .unwrap();
        let sites = |names: &[&str]| -> Vec<(String, i64)> {
            let names: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
            store
                .reference_sites_of_edges_to_folded_targets(&[Language::Php], &names)
                .unwrap()
                .into_iter()
                .map(|s| (s.file_path, s.site.map_or(0, |site| site.line)))
                .collect()
        };
        // Cross-file and same-file edges, from the languages asked for only.
        assert_eq!(sites(&["formatPRICE"]), [("view.php".to_string(), 2)]);
        assert_eq!(sites(&["LOCAL"]), [("view.php".to_string(), 3)]);
        assert!(sites(&["price"]).is_empty());
    }

    #[test]
    fn unresolved_refs_are_found_by_folded_names_and_qualified_php_classes() {
        let mut store = store("folded-names");
        store
            .upsert_nodes(&[
                node("function:src1", "src1", "a.php"),
                node("function:src2", "src2", "b.js"),
            ])
            .unwrap();
        let mk = |from: &str, name: &str, file: &str, lang: Language| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[
                mk("function:src1", "\\HELPER", "a.php", Language::Php),
                mk("function:src1", "\\helper", "a.php", Language::Php),
                mk("function:src1", "\\helperx", "a.php", Language::Php),
                mk("function:src1", "F\\FirstName.make", "a.php", Language::Php),
                mk("function:src1", "Plain.make", "a.php", Language::Php),
                mk("function:src2", "\\HELPER", "b.js", Language::JavaScript),
            ])
            .unwrap();
        let folded: Vec<String> = store
            .unresolved_refs_by_folded_names(&[Language::Php], &["\\Helper".to_string()])
            .unwrap()
            .into_iter()
            .map(|reference| reference.reference_name)
            .collect();
        // Any ASCII case of the whole name, and only the languages asked for.
        assert_eq!(folded, ["\\HELPER", "\\helper"]);
        assert!(
            store
                .unresolved_refs_by_folded_names(&[], &["\\helper".to_string()])
                .unwrap()
                .is_empty()
        );
        let qualified: Vec<String> = store
            .unresolved_php_calls_through_qualified_classes()
            .unwrap()
            .into_iter()
            .map(|reference| reference.reference_name)
            .collect();
        assert_eq!(qualified, ["F\\FirstName.make"]);
    }

    /// W5-09e: a reference resolution takes out of `unresolved_refs` moves
    /// whole into `resolved_refs`, keeping its id, once even when listed
    /// twice; with the rows still unresolved they are every reference of the
    /// file, which the stored reads return by id and without it, per file or
    /// per site.
    #[test]
    fn resolved_references_move_whole_and_read_back_as_extracted() {
        let mut store = store("resolved-refs-move");
        store
            .upsert_nodes(&[
                node("function:a", "a", "a.ts"),
                node("function:b", "b", "b.ts"),
            ])
            .unwrap();
        let mk = |from: &str, name: &str, line: i64, file: &str| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            col: 2,
            candidates: Some(vec!["x".to_string()]),
            file_path: file.to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        };
        let written = vec![
            mk("function:a", "one", 1, "a.ts"),
            mk("function:a", "two", 2, "a.ts"),
            mk("function:a", "two", 2, "a.ts"),
            mk("function:a", "three", 3, "a.ts"),
            mk("function:b", "other", 1, "b.ts"),
        ];
        store.insert_unresolved_refs(&written).unwrap();
        let ids: Vec<i64> = store
            .unresolved_refs_by_file_path("a.ts")
            .unwrap()
            .into_iter()
            .map(|reference| reference.id.unwrap())
            .collect();
        // The first `two` and `three` resolve; `three` is listed twice.
        store
            .delete_resolved_unresolved_refs(&[ids[1], ids[3], ids[3]])
            .unwrap();
        let moved: Vec<(i64, String)> = store
            .connection()
            .prepare("SELECT id, reference_name FROM resolved_refs ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            moved,
            [(ids[1], "two".to_string()), (ids[3], "three".to_string())]
        );
        let back = store.stored_refs_of_file("a.ts").unwrap();
        let expected: Vec<UnresolvedRef> = written[..4].to_vec();
        assert_eq!(back, expected, "every reference, by id, without it");
        let site = |from: &str, line: i64| ReferenceSite {
            from_node_id: from.to_string(),
            line,
            col: 2,
        };
        let at_sites = store
            .stored_refs_at_sites(&BTreeSet::from([site("function:a", 2)]))
            .unwrap();
        assert_eq!(at_sites, written[1..3].to_vec(), "the site's multiset");

        // A site replacement and a whole-file replacement clear the stored
        // references they replace; deleting a file's node cascades.
        store
            .replace_resolution_state_at_sites(&[site("function:a", 2)], &[])
            .unwrap();
        assert_eq!(
            store.stored_refs_of_file("a.ts").unwrap(),
            vec![written[0].clone(), written[3].clone()]
        );
        store
            .replace_resolution_state_from_file("a.ts", &written[..1])
            .unwrap();
        assert_eq!(
            store.stored_refs_of_file("a.ts").unwrap(),
            written[..1].to_vec()
        );
        let b_ids: Vec<i64> = store
            .unresolved_refs_by_file_path("b.ts")
            .unwrap()
            .into_iter()
            .map(|reference| reference.id.unwrap())
            .collect();
        store.delete_resolved_unresolved_refs(&b_ids).unwrap();
        store
            .connection()
            .execute("DELETE FROM nodes WHERE id = 'function:b'", [])
            .unwrap();
        assert!(store.stored_refs_of_file("b.ts").unwrap().is_empty());
    }

    /// W5-09a: the replacements of a sync's dependents applied in one
    /// transaction leave what applying them one by one does, each of them
    /// replacing what it covers: the stored references (resolved ones too)
    /// and resolution edges at a site, and of a whole file.
    #[test]
    fn batched_resolution_replacements_match_one_by_one() {
        let mk = |from: &str, name: &str, line: i64, file: &str| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        };
        let build = |name: &str| {
            let mut store = store(name);
            store
                .upsert_nodes(&[
                    node("function:a", "a", "a.ts"),
                    node("function:b", "b", "b.ts"),
                    node("function:t", "t", "t.ts"),
                ])
                .unwrap();
            store
                .insert_unresolved_refs(&[
                    mk("function:a", "x", 1, "a.ts"),
                    mk("function:a", "t", 2, "a.ts"),
                    mk("function:b", "t", 1, "b.ts"),
                ])
                .unwrap();
            // `t` resolves at both sites: its edges are written and its rows
            // move into `resolved_refs`.
            let resolved: Vec<i64> = ["a.ts", "b.ts"]
                .into_iter()
                .flat_map(|file| store.unresolved_refs_by_file_path(file).unwrap())
                .filter(|reference| reference.reference_name == "t")
                .map(|reference| reference.id.unwrap())
                .collect();
            let call = |source: &str, line: i64| Edge {
                line: Some(line),
                col: Some(0),
                ..edge(source, "function:t", EdgeKind::Calls)
            };
            store
                .insert_edges(&[call("function:a", 2), call("function:b", 1)])
                .unwrap();
            store.delete_resolved_unresolved_refs(&resolved).unwrap();
            store
        };
        let site = ReferenceSite {
            from_node_id: "function:a".to_string(),
            line: 2,
            col: 0,
        };
        let a_now = vec![mk("function:a", "u", 2, "a.ts")];
        let b_now = vec![mk("function:b", "v", 1, "b.ts")];
        let mut one = build("replace-one-by-one");
        one.replace_resolution_state_at_sites(std::slice::from_ref(&site), &a_now)
            .unwrap();
        one.replace_resolution_state_from_file("b.ts", &b_now)
            .unwrap();
        let mut batched = build("replace-batched");
        batched
            .replace_resolution_states(&[
                ResolutionReplacement::Sites {
                    sites: vec![site],
                    refs: a_now.clone(),
                },
                ResolutionReplacement::File {
                    file_path: "b.ts".to_string(),
                    refs: b_now.clone(),
                },
            ])
            .unwrap();
        let state = |store: &Store| {
            let edges: i64 = store
                .connection()
                .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
                .unwrap();
            (
                store.stored_refs_of_file("a.ts").unwrap(),
                store.stored_refs_of_file("b.ts").unwrap(),
                edges,
            )
        };
        assert_eq!(state(&batched), state(&one));
        assert_eq!(
            state(&batched),
            (
                vec![mk("function:a", "x", 1, "a.ts"), a_now[0].clone()],
                b_now,
                0
            ),
            "each replacement replaced what it covers"
        );
    }

    /// W5-09g: a move spanning several chunks, with ids repeated inside a
    /// chunk and across chunks, leaves both tables as moving the rows one by
    /// one does: each listed row moved once, whole and with its id, and every
    /// other row still unresolved.
    #[test]
    fn a_move_across_chunks_moves_each_listed_row_once() {
        let mut store = store("resolved-refs-chunks");
        store
            .upsert_nodes(&[node("function:a", "a", "a.ts")])
            .unwrap();
        let refs: Vec<UnresolvedRef> = (0..3_000)
            .map(|i| UnresolvedRef {
                id: None,
                from_node_id: "function:a".to_string(),
                reference_name: format!("r{i}"),
                reference_kind: EdgeKind::Calls,
                line: i + 1,
                col: 0,
                candidates: None,
                file_path: "a.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            })
            .collect();
        store.insert_unresolved_refs(&refs).unwrap();
        let ids: Vec<i64> = store
            .unresolved_refs_by_file_path("a.ts")
            .unwrap()
            .into_iter()
            .map(|reference| reference.id.unwrap())
            .collect();
        // Every third row, in reverse, then a repeat of some of them: the
        // repeats land in the same chunk and in later ones.
        let mut listed: Vec<i64> = ids.iter().copied().step_by(3).rev().collect();
        listed.extend(ids.iter().copied().step_by(9));
        assert!(listed.len() > 2 * SQLITE_PARAM_CHUNK_SIZE);
        store.delete_resolved_unresolved_refs(&listed).unwrap();

        let id_list = |sql: &str| -> Vec<i64> {
            store
                .connection()
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let moved: BTreeSet<i64> = ids.iter().copied().step_by(3).collect();
        assert_eq!(
            id_list("SELECT id FROM resolved_refs ORDER BY id"),
            moved.iter().copied().collect::<Vec<_>>()
        );
        assert_eq!(
            id_list("SELECT id FROM unresolved_refs ORDER BY id"),
            ids.iter()
                .copied()
                .filter(|id| !moved.contains(id))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store.stored_refs_of_file("a.ts").unwrap(),
            refs,
            "every row, whole, by id"
        );
    }

    #[test]
    fn the_one_pass_retry_reads_what_the_four_lookups_read() {
        let mut store = store("one-pass-retry");
        store
            .upsert_nodes(&[
                node("function:php", "php", "a.php"),
                node("function:pas", "pas", "b.pas"),
                node("function:js", "js", "c.js"),
            ])
            .unwrap();
        let mk = |from: &str, name: &str, kind: EdgeKind, lang: Language| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            col: 0,
            candidates: None,
            file_path: String::new(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        };
        let php = |name: &str, kind| mk("function:php", name, kind, Language::Php);
        let pas = |name: &str| mk("function:pas", name, EdgeKind::Calls, Language::Pascal);
        let js = |name: &str, kind| mk("function:js", name, kind, Language::JavaScript);
        let mut function_ref = js("util.helper", EdgeKind::References);
        function_ref.is_function_ref = true;
        store
            .insert_unresolved_refs(&[
                php("\\HELPER", EdgeKind::Calls),
                php("\\helperx", EdgeKind::Calls),
                php("Helper", EdgeKind::Calls),
                php("F\\FirstName.make", EdgeKind::Calls),
                php("F\\Other.helper", EdgeKind::Calls),
                php("F\\Other.other", EdgeKind::Calls),
                php("F\\FirstName.make", EdgeKind::References),
                pas("HELPER"),
                pas("Unit.Helper"),
                js("\\HELPER", EdgeKind::Calls),
                js("HELPER", EdgeKind::Calls),
                js("a.helper", EdgeKind::Calls),
                js("b.helper", EdgeKind::Calls),
                js("c.helper", EdgeKind::Calls),
                function_ref,
                js("./lib/helper.js", EdgeKind::Imports),
                js("mod:make/2", EdgeKind::Calls),
                js("x.make", EdgeKind::Calls),
                js("helper", EdgeKind::Calls),
            ])
            .unwrap();
        let folded_languages = [Language::Php, Language::Pascal, Language::Cfml];
        let ids = |refs: Vec<UnresolvedRef>| -> Vec<i64> {
            refs.into_iter()
                .map(|reference| reference.id.unwrap())
                .collect()
        };
        let four = |names: &[String], ceiling: usize| -> Vec<i64> {
            let rooted: Vec<String> = names.iter().map(|name| format!("\\{name}")).collect();
            let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
            let mut out = BTreeSet::new();
            out.extend(ids(store
                .unresolved_refs_by_name_tails(names, ceiling)
                .unwrap()));
            out.extend(ids(store
                .unresolved_refs_by_folded_names(&[Language::Php], &rooted)
                .unwrap()));
            out.extend(ids(store
                .unresolved_refs_by_folded_names(&folded_languages, names)
                .unwrap()));
            if !names.is_empty() {
                out.extend(ids(store
                    .unresolved_php_calls_through_qualified_classes()
                    .unwrap()
                    .into_iter()
                    .filter(|reference| {
                        reference
                            .reference_name
                            .rsplit_once('.')
                            .is_some_and(|(class, member)| {
                                wanted.contains(member)
                                    || class
                                        .rsplit('\\')
                                        .next()
                                        .is_some_and(|class| wanted.contains(class))
                            })
                    })
                    .collect()));
            }
            out.into_iter().collect()
        };
        let one = |names: &[String], ceiling: usize| -> Vec<String> {
            store
                .unresolved_refs_retried_by_names(names, ceiling, &folded_languages)
                .unwrap()
                .into_iter()
                .map(|reference| reference.reference_name)
                .collect()
        };
        let names =
            |list: &[&str]| -> Vec<String> { list.iter().map(|n| (*n).to_string()).collect() };
        for list in [
            &["helper"][..],
            &["Helper"],
            &["make"],
            &["FirstName"],
            &["helper", "make"],
            &["Other"],
            &["nothing"],
            &[],
        ] {
            for ceiling in [1, 3, 500] {
                let names = names(list);
                let read = store
                    .unresolved_refs_retried_by_names(&names, ceiling, &folded_languages)
                    .unwrap();
                assert_eq!(ids(read), four(&names, ceiling), "{list:?} at {ceiling}");
            }
        }
        // Not vacuous: each lane reads its own rows.
        assert_eq!(
            one(&names(&["helper"]), 500),
            [
                "\\HELPER",
                "Helper",
                "F\\Other.helper",
                "HELPER",
                "a.helper",
                "b.helper",
                "c.helper",
                "util.helper",
                "./lib/helper.js",
            ]
        );
        // Six qualified references share the tail `helper`: past a ceiling of
        // three the tail lane reads none of them, the PHP class lane still
        // its own. A tail matches in its own case only.
        assert_eq!(
            one(&names(&["helper"]), 3),
            ["\\HELPER", "Helper", "F\\Other.helper", "HELPER"]
        );
        assert_eq!(
            one(&names(&["Helper"]), 500),
            ["\\HELPER", "Helper", "HELPER", "Unit.Helper"]
        );
        assert_eq!(
            one(&names(&["make"]), 500),
            [
                "F\\FirstName.make",
                "F\\FirstName.make",
                "mod:make/2",
                "x.make"
            ]
        );
        assert_eq!(one(&names(&["FirstName"]), 500), ["F\\FirstName.make"]);
    }

    #[test]
    fn unresolved_refs_of_a_language_are_found_by_their_member() {
        let mut store = store("by-members");
        store
            .upsert_nodes(&[
                node("function:src1", "src1", "a.php"),
                node("function:src2", "src2", "b.rb"),
            ])
            .unwrap();
        let mk = |from: &str, name: &str, file: &str, lang: Language| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[
                mk("function:src1", "page.save", "a.php", Language::Php),
                mk("function:src1", "this.repo.save", "a.php", Language::Php),
                mk("function:src1", "page.saveAll", "a.php", Language::Php),
                mk("function:src1", "save", "a.php", Language::Php),
                mk("function:src1", "x.a_b", "a.php", Language::Php),
                mk("function:src1", "x.a*b", "a.php", Language::Php),
                mk("function:src2", "page.save", "b.rb", Language::Ruby),
            ])
            .unwrap();
        let names = |members: &[&str]| -> Vec<String> {
            let members: Vec<String> = members.iter().map(|m| (*m).to_string()).collect();
            store
                .unresolved_refs_of_language_by_members(Language::Php, &members)
                .unwrap()
                .into_iter()
                .map(|reference| reference.reference_name)
                .collect()
        };
        // The member after the last `.`, whole: not a bare name, not a longer
        // member, not another language's.
        assert_eq!(names(&["save"]), ["page.save", "this.repo.save"]);
        // GLOB's own characters match only themselves.
        assert_eq!(names(&["a*b"]), ["x.a*b"]);
        assert_eq!(
            names(&["a_b", "save"]),
            ["page.save", "this.repo.save", "x.a_b"]
        );
        assert!(names(&[]).is_empty());
        assert!(names(&["repo.save"]).is_empty());
    }

    #[test]
    fn function_ref_flag_round_trips_through_unresolved_refs() {
        let mut store = store("fn-ref");
        store
            .upsert_nodes(&[node("function:src", "src", "a.rs")])
            .unwrap();
        let unresolved = UnresolvedRef {
            id: None,
            from_node_id: "function:src".to_string(),
            reference_name: "callback".to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            col: 0,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: true,
            reference_subkind: Some(ReferenceSubkind::Autoload),
        };
        store
            .insert_unresolved_refs(std::slice::from_ref(&unresolved))
            .unwrap();
        let read = store.all_unresolved_refs().unwrap();
        assert_eq!(read.len(), 1);
        assert!(read[0].is_function_ref);
        assert_eq!(read[0].reference_kind, EdgeKind::References);
        assert_eq!(read[0].reference_subkind, Some(ReferenceSubkind::Autoload));
    }

    #[test]
    fn insert_unresolved_refs_empty_is_noop() {
        let mut store = store("unresolved-empty");
        store.insert_unresolved_refs(&[]).unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 0);
    }

    /// Two sibling call sites — SAME `from_node_id`, SAME `reference_name`, SAME
    /// `reference_kind`, differing only in `line` — must be independently
    /// deletable. The old `(from,name,kind)` key could not tell them apart and,
    /// carrying no `LIMIT`, removed both when one resolved (#1269/#1270).
    #[test]
    fn delete_resolved_refs_by_row_id_removes_only_the_named_row() {
        let mut store = store("delete-resolved-siblings");
        store
            .upsert_nodes(&[node("function:src", "src", "a.rs")])
            .unwrap();
        let mk = |name: &str, line: i64| UnresolvedRef {
            id: None,
            from_node_id: "function:src".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            col: 0,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[mk("helper", 2), mk("helper", 3), mk("Other", 4)])
            .unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 3);

        store.delete_resolved_unresolved_refs(&[]).unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 3);

        let siblings: Vec<UnresolvedRef> = store
            .all_unresolved_refs()
            .unwrap()
            .into_iter()
            .filter(|r| r.reference_name == "helper")
            .collect();
        assert_eq!(siblings.len(), 2);
        let first = siblings[0].id.unwrap();
        let second = siblings[1].id.unwrap();
        assert_ne!(first, second);

        store.delete_resolved_unresolved_refs(&[first]).unwrap();

        let remaining = store.all_unresolved_refs().unwrap();
        assert_eq!(remaining.len(), 2);
        let survivor = remaining
            .iter()
            .find(|r| r.reference_name == "helper")
            .expect("the sibling `helper` row must survive its twin's deletion");
        assert_eq!(survivor.id, Some(second));
        assert_eq!(survivor.line, siblings[1].line);

        store.delete_resolved_unresolved_refs(&[second]).unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 1);

        store.delete_resolved_unresolved_refs(&[second]).unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 1);
    }

    #[test]
    fn dependency_and_dependent_file_paths_cross_file_only() {
        let mut store = store("file-deps");
        store
            .upsert_nodes(&[
                node("function:caller", "caller", "a.rs"),
                node("function:callee", "callee", "b.rs"),
                node("function:local", "local", "a.rs"),
            ])
            .unwrap();
        store
            .insert_edges(&[
                edge("function:caller", "function:callee", EdgeKind::Calls),
                edge("function:caller", "function:local", EdgeKind::Contains),
            ])
            .unwrap();

        assert_eq!(
            store.dependent_file_paths("b.rs").unwrap(),
            vec!["a.rs".to_string()]
        );
        assert_eq!(
            store.dependency_file_paths("a.rs").unwrap(),
            vec!["b.rs".to_string()]
        );
        assert!(store.dependent_file_paths("a.rs").unwrap().is_empty());
    }

    #[test]
    fn dependent_file_paths_unresolved_matches_godot_subkinds_only() {
        let mut store = store("unresolved-deps");
        store
            .upsert_nodes(&[
                node("godot:scene", "CharacterBase", "scenes/character_base.tscn"),
                node("godot:scene2", "EyeDragon", "scenes/EyeDragon.tscn"),
                node("godot:project", "project", "project.godot"),
                node("function:rust", "user", "src/main.rs"),
            ])
            .unwrap();
        let mk = |from: &str,
                  name: &str,
                  file: &str,
                  lang: Language,
                  subkind: Option<ReferenceSubkind>| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: subkind,
        };
        store
            .insert_unresolved_refs(&[
                mk(
                    "godot:scene",
                    "Scripts/Component/health_component.gd",
                    "scenes/character_base.tscn",
                    Language::GodotScene,
                    Some(ReferenceSubkind::ScriptAttach),
                ),
                mk(
                    "godot:scene2",
                    "res://Scripts/Component/health_component.gd",
                    "scenes/EyeDragon.tscn",
                    Language::GodotScene,
                    Some(ReferenceSubkind::ScriptAttach),
                ),
                mk(
                    "godot:project",
                    "Scripts/Component/health_component.gd",
                    "project.godot",
                    Language::GodotProject,
                    Some(ReferenceSubkind::Autoload),
                ),
                mk(
                    "function:rust",
                    "Scripts/Component/health_component.gd",
                    "src/main.rs",
                    Language::Rust,
                    None,
                ),
            ])
            .unwrap();

        let mut referrers = store
            .dependent_file_paths_unresolved("Scripts/Component/health_component.gd")
            .unwrap();
        referrers.sort();
        referrers.dedup();
        assert_eq!(
            referrers,
            vec![
                "project.godot".to_string(),
                "scenes/EyeDragon.tscn".to_string(),
                "scenes/character_base.tscn".to_string(),
            ],
            "res:// scheme normalized and all Godot subkinds matched; the Rust ref (NULL subkind) excluded"
        );

        assert!(
            store
                .dependent_file_paths_unresolved("Scripts/Component/other.gd")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dependent_file_paths_unresolved_excludes_gdscript_load_path_and_non_godot() {
        let mut store = store("unresolved-deps-exclude");
        store
            .upsert_nodes(&[
                node("function:gd", "loader", "scripts/loader.gd"),
                node("function:py", "importer", "app/importer.py"),
            ])
            .unwrap();
        let mk = |from: &str, file: &str, lang: Language, subkind: Option<ReferenceSubkind>| {
            UnresolvedRef {
                id: None,
                from_node_id: from.to_string(),
                reference_name: "scripts/target.gd".to_string(),
                reference_kind: EdgeKind::References,
                line: 1,
                col: 0,
                candidates: None,
                file_path: file.to_string(),
                language: lang,
                is_function_ref: false,
                reference_subkind: subkind,
            }
        };
        store
            .insert_unresolved_refs(&[
                mk(
                    "function:gd",
                    "scripts/loader.gd",
                    Language::Gdscript,
                    Some(ReferenceSubkind::GdscriptLoadPath),
                ),
                mk("function:py", "app/importer.py", Language::Python, None),
            ])
            .unwrap();

        assert!(
            store
                .dependent_file_paths_unresolved("scripts/target.gd")
                .unwrap()
                .is_empty(),
            "gdscript_load_path (already an edge) and non-Godot refs must not be returned"
        );
    }

    #[test]
    fn dependent_file_paths_via_import_name_surfaces_res_loaders_only() {
        let mut store = store("import-name-loaders");
        let mut scene_loader = node("import:scene", "res://Scenes/BaseStage.tscn", "loader.gd");
        scene_loader.kind = NodeKind::Import;
        let mut other_loader = node("import:other", "res://Scenes/Other.tscn", "other.gd");
        other_loader.kind = NodeKind::Import;
        let mut bare_import = node("import:bare", "Scenes/BaseStage.tscn", "bare.gd");
        bare_import.kind = NodeKind::Import;
        store
            .upsert_nodes(&[scene_loader, other_loader, bare_import])
            .unwrap();

        let mut referrers = store
            .dependent_file_paths_via_import_name("Scenes/BaseStage.tscn")
            .unwrap();
        referrers.sort();
        assert_eq!(
            referrers,
            vec!["loader.gd".to_string()],
            "only the res://-scheme import node whose name equals the target surfaces its loader"
        );
        assert!(
            store
                .dependent_file_paths_via_import_name("Scenes/Missing.tscn")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dependent_file_paths_main_scene_surfaces_untagged_project_godot_only() {
        let mut store = store("main-scene-lane");
        store
            .upsert_nodes(&[
                node("godot:project", "project", "project.godot"),
                node("function:rust", "user", "src/main.rs"),
            ])
            .unwrap();
        let mk = |from: &str,
                  name: &str,
                  file: &str,
                  lang: Language,
                  subkind: Option<ReferenceSubkind>| UnresolvedRef {
            id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            col: 0,
            candidates: None,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: subkind,
        };
        store
            .insert_unresolved_refs(&[
                mk(
                    "godot:project",
                    "res://scenes/main_menu.tscn",
                    "project.godot",
                    Language::GodotProject,
                    None,
                ),
                mk(
                    "function:rust",
                    "scenes/main_menu.tscn",
                    "src/main.rs",
                    Language::Rust,
                    None,
                ),
            ])
            .unwrap();

        let referrers = store
            .dependent_file_paths_main_scene("scenes/main_menu.tscn")
            .unwrap();
        assert_eq!(
            referrers,
            vec!["project.godot".to_string()],
            "the untagged main_scene ref surfaces its project.godot origin; the non-Godot None ref is excluded by the origin gate"
        );

        let mut unresolved = store
            .dependent_file_paths_unresolved("scenes/main_menu.tscn")
            .unwrap();
        unresolved.sort();
        assert!(
            unresolved.is_empty(),
            "the 6-subkind allow-list still excludes the untagged main_scene ref (byte-unchanged)"
        );
    }

    #[test]
    fn delete_resolved_edges_and_named_target_sources() {
        let mut store = store("resolved-edges");
        store
            .upsert_nodes(&[
                node("function:caller", "caller", "a.rs"),
                node("function:callee", "callee", "b.rs"),
            ])
            .unwrap();
        store
            .insert_edges(&[
                edge("function:caller", "function:callee", EdgeKind::Calls),
                edge("function:caller", "function:callee", EdgeKind::Contains),
            ])
            .unwrap();

        assert!(
            store
                .source_files_of_edges_to_named_targets(&[])
                .unwrap()
                .is_empty()
        );
        let sources = store
            .source_files_of_edges_to_named_targets(&["callee".to_string()])
            .unwrap();
        assert_eq!(sources, vec!["a.rs".to_string()]);

        let removed = store.delete_resolved_edges_from_file("a.rs").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.all_edges().unwrap().len(), 1);
        assert_eq!(store.all_edges().unwrap()[0].kind, EdgeKind::Contains);
    }

    #[test]
    fn whole_file_fallbacks_keep_same_file_value_references() {
        let mut store = store("value-refs-survive");
        store
            .upsert_nodes(&[
                node("function:run", "run", "a.ts"),
                node("constant:limit", "LIMIT_MAX", "a.ts"),
                node("function:callee", "callee", "b.ts"),
            ])
            .unwrap();
        let mut value_ref = edge("function:run", "constant:limit", EdgeKind::References);
        value_ref.metadata = Some(serde_json::json!({ "valueRef": true }));
        value_ref.line = None;
        value_ref.col = None;
        // A resolved `references` edge without the marker still goes.
        let mut resolved_ref = edge("function:run", "function:callee", EdgeKind::References);
        resolved_ref.line = Some(3);
        resolved_ref.col = Some(4);
        store
            .insert_edges(&[
                value_ref.clone(),
                resolved_ref,
                edge("function:run", "function:callee", EdgeKind::Calls),
            ])
            .unwrap();
        assert_eq!(store.delete_resolved_edges_from_file("a.ts").unwrap(), 2);
        let left = store.all_edges().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].target, "constant:limit");

        store
            .insert_edges(&[edge("function:run", "function:callee", EdgeKind::Calls)])
            .unwrap();
        assert_eq!(
            store
                .replace_resolution_state_from_file("a.ts", &[])
                .unwrap(),
            1
        );
        let left = store.all_edges().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, EdgeKind::References);
    }

    #[test]
    fn dependent_ref_narrowing_site_groups_are_read_and_deleted_atomically() {
        let mut store = store("dependent-ref-narrowing-sites");
        store
            .upsert_nodes(&[
                node("function:caller", "caller", "a.rs"),
                node("function:other", "other", "other.rs"),
                node("function:fallback", "fallback", "fallback.rs"),
                node("function:self", "self_call", "b.rs"),
                node("function:callee", "callee", "b.rs"),
                node("function:sibling", "sibling", "b.rs"),
            ])
            .unwrap();

        let at =
            |source: &str, target: &str, kind: EdgeKind, line: Option<i64>, col: Option<i64>| {
                let mut edge = edge(source, target, kind);
                edge.line = line;
                edge.col = col;
                edge
            };
        store
            .insert_edges(&[
                at(
                    "function:caller",
                    "function:callee",
                    EdgeKind::Calls,
                    Some(10),
                    Some(2),
                ),
                at(
                    "function:caller",
                    "function:sibling",
                    EdgeKind::References,
                    Some(10),
                    Some(2),
                ),
                at(
                    "function:caller",
                    "function:callee",
                    EdgeKind::Calls,
                    Some(11),
                    Some(2),
                ),
                at(
                    "function:other",
                    "function:callee",
                    EdgeKind::Calls,
                    Some(10),
                    Some(2),
                ),
                at(
                    "function:caller",
                    "function:callee",
                    EdgeKind::Contains,
                    Some(10),
                    Some(2),
                ),
                at(
                    "function:fallback",
                    "function:callee",
                    EdgeKind::Calls,
                    None,
                    None,
                ),
                at(
                    "function:self",
                    "function:callee",
                    EdgeKind::Calls,
                    Some(10),
                    Some(2),
                ),
            ])
            .unwrap();

        let caller_site = ReferenceSite {
            from_node_id: "function:caller".to_string(),
            line: 10,
            col: 2,
        };
        let incoming = store.reference_sites_dependent_on_file("b.rs").unwrap();
        assert!(incoming.contains(&FileReferenceSite {
            file_path: "a.rs".to_string(),
            site: Some(caller_site.clone()),
        }));
        assert!(incoming.contains(&FileReferenceSite {
            file_path: "fallback.rs".to_string(),
            site: None,
        }));
        assert!(
            incoming.iter().all(|entry| entry.file_path != "b.rs"),
            "same-file edges are not dependent refresh sites"
        );
        assert_eq!(
            incoming
                .iter()
                .filter(
                    |entry| entry.file_path == "a.rs" && entry.site.as_ref() == Some(&caller_site)
                )
                .count(),
            1,
            "two sibling edges at one source site must deduplicate to one group"
        );

        let named = store
            .reference_sites_of_edges_to_named_targets(&[
                "callee".to_string(),
                "sibling".to_string(),
                "callee".to_string(),
            ])
            .unwrap();
        assert!(named.contains(&FileReferenceSite {
            file_path: "a.rs".to_string(),
            site: Some(caller_site.clone()),
        }));
        assert!(named.contains(&FileReferenceSite {
            file_path: "fallback.rs".to_string(),
            site: None,
        }));
        assert!(
            named.iter().all(|entry| entry.file_path != "b.rs"),
            "the cross-file query leaves same-file edges out"
        );
        let same_file = store
            .reference_sites_of_same_file_edges_to_named_targets(&[
                "callee".to_string(),
                "sibling".to_string(),
            ])
            .unwrap();
        assert_eq!(
            same_file,
            vec![FileReferenceSite {
                file_path: "b.rs".to_string(),
                site: Some(ReferenceSite {
                    from_node_id: "function:self".to_string(),
                    line: 10,
                    col: 2,
                }),
            }],
            "the same-file query returns exactly the edge whose source shares its target's file"
        );

        let unresolved = |name: &str, line: i64| UnresolvedRef {
            id: None,
            from_node_id: "function:caller".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            col: 2,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[
                unresolved("resolved-sibling", 10),
                unresolved("no-edge-sibling", 10),
                unresolved("unrelated", 12),
            ])
            .unwrap();
        let unrelated_before = store
            .unresolved_refs_by_names(&["unrelated".to_string()])
            .unwrap()[0]
            .id;
        let selected = store
            .unresolved_refs_by_sites(&[caller_site.clone(), caller_site.clone()])
            .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|reference| reference.reference_name.as_str())
                .collect::<Vec<_>>(),
            vec!["resolved-sibling", "no-edge-sibling"],
            "resolved and no-edge sibling refs at one site must be selected together"
        );

        assert_eq!(
            store
                .delete_resolved_edges_at_sites(&[caller_site.clone(), caller_site.clone()])
                .unwrap(),
            2,
            "both non-contains sibling edges at the site must be deleted"
        );
        let remaining_edges = store.all_edges().unwrap();
        assert!(remaining_edges.iter().any(|edge| {
            edge.source == "function:caller"
                && edge.line == Some(10)
                && edge.kind == EdgeKind::Contains
        }));
        assert!(remaining_edges.iter().any(|edge| {
            edge.source == "function:caller"
                && edge.line == Some(11)
                && edge.kind == EdgeKind::Calls
        }));
        assert!(remaining_edges.iter().any(|edge| {
            edge.source == "function:other" && edge.line == Some(10) && edge.kind == EdgeKind::Calls
        }));

        assert_eq!(
            store
                .delete_unresolved_refs_at_sites(&[caller_site.clone(), caller_site])
                .unwrap(),
            2,
            "both persisted sibling refs at the site must be deleted"
        );
        let unrelated_after = store
            .unresolved_refs_by_names(&["unrelated".to_string()])
            .unwrap();
        assert_eq!(unrelated_after.len(), 1);
        assert_eq!(
            unrelated_after[0].id, unrelated_before,
            "an unrelated unresolved row must retain physical identity"
        );
    }

    fn requeue_fixture(test_name: &str) -> (Store, ReferenceSite, UnresolvedRef) {
        let mut store = store(test_name);
        store
            .upsert_nodes(&[
                node("function:caller", "caller", "a.rs"),
                node("function:callee", "callee", "b.rs"),
            ])
            .unwrap();
        let mut old_edge = edge("function:caller", "function:callee", EdgeKind::Calls);
        old_edge.line = Some(10);
        old_edge.col = Some(2);
        store.insert_edges(&[old_edge]).unwrap();
        let old_ref = UnresolvedRef {
            id: None,
            from_node_id: "function:caller".to_string(),
            reference_name: "old".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 10,
            col: 2,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(std::slice::from_ref(&old_ref))
            .unwrap();
        store
            .connection()
            .execute_batch(
                r#"CREATE TRIGGER interrupt_fresh_requeue
                BEFORE INSERT ON unresolved_refs
                WHEN NEW.reference_name = 'fresh'
                BEGIN
                  SELECT RAISE(ABORT, 'forced requeue interruption');
                END;"#,
            )
            .unwrap();
        (
            store,
            ReferenceSite {
                from_node_id: "function:caller".to_string(),
                line: 10,
                col: 2,
            },
            UnresolvedRef {
                reference_name: "fresh".to_string(),
                ..old_ref
            },
        )
    }

    fn assert_requeue_rollback(store: &Store) {
        let edges = store.all_edges().unwrap();
        assert_eq!(edges.len(), 1, "the last committed edge must survive");
        assert_eq!(edges[0].source, "function:caller");
        assert_eq!(edges[0].target, "function:callee");
        assert_eq!(edges[0].line, Some(10));
        let refs = store.unresolved_refs_by_file_path("a.rs").unwrap();
        assert_eq!(refs.len(), 1, "the old unresolved sibling must survive");
        assert_eq!(refs[0].reference_name, "old");
    }

    #[test]
    fn file_requeue_rolls_back_edge_and_ref_deletion_when_insert_fails() {
        let (mut store, _site, fresh) = requeue_fixture("file-requeue-rollback");
        let error = store
            .replace_resolution_state_from_file("a.rs", &[fresh])
            .expect_err("the trigger must interrupt the requeue");
        assert!(error.to_string().contains("forced requeue interruption"));
        assert_requeue_rollback(&store);
    }

    #[test]
    fn site_requeue_rolls_back_edge_and_ref_deletion_when_insert_fails() {
        let (mut store, site, fresh) = requeue_fixture("site-requeue-rollback");
        let error = store
            .replace_resolution_state_at_sites(&[site], &[fresh])
            .expect_err("the trigger must interrupt the requeue");
        assert!(error.to_string().contains("forced requeue interruption"));
        assert_requeue_rollback(&store);
    }

    #[test]
    fn counts_reflects_nodes_edges_files() {
        let mut store = store("counts");
        store.upsert_file(&file("a.rs")).unwrap();
        store
            .upsert_nodes(&[
                node("function:a", "a", "a.rs"),
                node("function:b", "b", "a.rs"),
            ])
            .unwrap();
        store
            .insert_edges(&[edge("function:a", "function:b", EdgeKind::Calls)])
            .unwrap();
        let counts = store.counts().unwrap();
        assert_eq!(counts.node_count, 2);
        assert_eq!(counts.edge_count, 1);
        assert_eq!(counts.file_count, 1);
    }

    #[test]
    fn compact_and_bulk_pragmas_preserve_content() {
        let mut store = store("compact");
        store
            .upsert_nodes(&[node("function:a", "a", "a.rs")])
            .unwrap();
        store.set_bulk_index_pragmas().unwrap();
        store.restore_default_pragmas().unwrap();
        store.compact().unwrap();
        assert_eq!(store.counts().unwrap().node_count, 1);
    }

    #[test]
    fn upsert_node_updates_existing_row_in_place() {
        let mut store = store("upsert-update");
        let mut n = node("function:u", "before", "u.rs");
        store.upsert_nodes(std::slice::from_ref(&n)).unwrap();
        n.name = "after".to_string();
        n.qualified_name = "u.rs::after".to_string();
        n.return_type = Some("i32".to_string());
        store.upsert_nodes(std::slice::from_ref(&n)).unwrap();
        assert_eq!(store.counts().unwrap().node_count, 1);
        let read = store.node_by_id("function:u").unwrap().unwrap();
        assert_eq!(read.name, "after");
        assert_eq!(read.return_type, Some("i32".to_string()));
    }

    #[test]
    fn delete_nodes_by_file_path_removes_all_file_nodes() {
        let mut store = store("delete-by-file");
        store
            .upsert_nodes(&[
                node("function:a", "a", "x.rs"),
                node("function:b", "b", "x.rs"),
                node("function:c", "c", "y.rs"),
            ])
            .unwrap();
        let removed = store.delete_nodes_by_file_path("x.rs").unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.node_count_by_file_path("x.rs").unwrap(), 0);
        assert_eq!(store.node_count_by_file_path("y.rs").unwrap(), 1);
    }

    #[test]
    fn node_by_id_missing_returns_none() {
        let store = store("missing-node");
        assert_eq!(store.node_by_id("nope").unwrap(), None);
    }
}
