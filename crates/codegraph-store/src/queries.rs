use std::collections::HashMap;

use codegraph_core::types::{
    Edge, EdgeKind, FileRecord, Language, Node, NodeKind, ReferenceSubkind, UnresolvedRef,
};
use rusqlite::{
    Connection, OptionalExtension, Row, ToSql, TransactionBehavior, named_params, params,
};
use serde_json::Value;

use crate::connection::Store;

const SQLITE_PARAM_CHUNK_SIZE: usize = 500;

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
    /// Callers pass `lowercase`; the SQL shape stays `lower(name) = ?` for `idx_nodes_lower_name`.
    pub fn nodes_by_lower_name(&self, lower_name: &str) -> rusqlite::Result<Vec<Node>> {
        query_nodes(
            &self.conn,
            "SELECT * FROM nodes WHERE lower(name) = ?",
            [lower_name],
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
    /// `name = ? COLLATE NOCASE` plus optional kind/language `IN (...)`; `LIMIT 20`.
    pub fn nodes_by_exact_name_nocase(
        &self,
        name: &str,
        kinds: &[NodeKind],
        languages: &[Language],
    ) -> rusqlite::Result<Vec<Node>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE name = ? COLLATE NOCASE");
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

    /// Ports `getAllNodeNames` from `upstream db/queries.ts:1655-1661`.
    /// `SELECT DISTINCT name FROM nodes` — the candidate name set for fuzzy fallback.
    pub fn all_node_names(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT name FROM nodes")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>("name"))?;
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
            INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
            VALUES (@path, @contentHash, @language, @size, @modifiedAt, @indexedAt, @nodeCount, @errors)
            ON CONFLICT(path) DO UPDATE SET
              content_hash = @contentHash,
              language = @language,
              size = @size,
              modified_at = @modifiedAt,
              indexed_at = @indexedAt,
              node_count = @nodeCount,
              errors = @errors
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
        let from_ids = refs
            .iter()
            .map(|unresolved| unresolved.from_node_id.as_str())
            .collect::<Vec<_>>();

        // Snapshot `from_node_id` existence INSIDE a `BEGIN IMMEDIATE` transaction
        // (see `insert_edges`): a concurrent writer deleting the source node
        // between an out-of-transaction snapshot and the insert would trip
        // `FOREIGN KEY constraint failed` and abort the sync. Dropping a
        // genuinely-absent source stays byte-identical to before.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_node_ids = existing_node_ids(&tx, &from_ids)?;
        {
            let mut stmt = tx.prepare_cached(
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
        }
        tx.commit()
    }

    /// Ports `getUnresolvedReferences` from `upstream db/queries.ts:1588-1603`.
    pub fn all_unresolved_refs(&self) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.conn.prepare("SELECT * FROM unresolved_refs")?;
        let rows = stmt.query_map([], row_to_unresolved_ref)?;
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

    /// Count rows currently in `unresolved_refs`. Sizes the resolve-phase
    /// progress bar; read AFTER framework refs are inserted or the bar overflows.
    pub fn unresolved_refs_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT count(*) FROM unresolved_refs", [], |row| row.get(0))
    }

    /// Ports `deleteSpecificResolvedReferences` from `upstream db/queries.ts:1716-1727`.
    /// Deletes one row per `(from_node_id, reference_name, reference_kind)` tuple in a single
    /// transaction, matching the upstream precise per-tuple delete so only actually-resolved refs go.
    pub fn delete_resolved_unresolved_refs(
        &mut self,
        keys: &[(String, String, EdgeKind)],
    ) -> rusqlite::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ?",
            )?;
            for (from_node_id, reference_name, reference_kind) in keys {
                stmt.execute(params![
                    from_node_id,
                    reference_name,
                    reference_kind.as_str()
                ])?;
            }
        }
        tx.commit()
    }

    /// Per-batch variant of [`Self::delete_resolved_unresolved_refs`] bounded by
    /// `max_id`: deletes only rows whose `id <= max_id`. The batched resolver
    /// reads refs in ascending-`id` order, so a duplicate `(from,name,kind)`
    /// tuple in a LATER batch has `id > max_id` and is preserved — letting the
    /// caller drop each batch's keys immediately instead of accumulating every
    /// resolved key for one final delete (bounds peak memory on large graphs).
    pub fn delete_resolved_unresolved_refs_up_to(
        &mut self,
        keys: &[(String, String, EdgeKind)],
        max_id: i64,
    ) -> rusqlite::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ? AND id <= ?",
            )?;
            for (from_node_id, reference_name, reference_kind) in keys {
                stmt.execute(params![
                    from_node_id,
                    reference_name,
                    reference_kind.as_str(),
                    max_id
                ])?;
            }
        }
        tx.commit()
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
    /// `OFF` and grow the page cache and mmap window. This trades crash durability
    /// for throughput during the one-shot full index, which is always safe because
    /// the full-index path starts from an empty DB (`index --force` removes the file
    /// first) and is re-runnable from scratch on failure.
    ///
    /// `synchronous` is a durability knob only: it never alters committed content,
    /// so golden byte-equivalence is preserved. MUST be scoped to the CLI full-index
    /// connection — never the shared `Store::open` defaults used by sync/daemon/watch.
    /// Pair with [`Store::restore_default_pragmas`] (via a Drop guard) so the restore
    /// runs on both the happy and the error path.
    pub fn set_bulk_index_pragmas(&self) -> rusqlite::Result<()> {
        self.conn.pragma_update(None, "synchronous", "OFF")?;
        self.conn.pragma_update(None, "cache_size", -262_144)?;
        self.conn
            .pragma_update(None, "mmap_size", 1_073_741_824_i64)?;
        Ok(())
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
    /// `file_path`. Every non-`contains` edge is produced by reference resolution
    /// (it carries `metadata.resolvedBy`); `contains` edges come only from
    /// extraction. Used by the incremental sync path to drop a refreshed file's
    /// outgoing resolved edges (intra-file AND cross-file) before its refreshed
    /// references are re-resolved, so rebuilding them cannot duplicate a surviving
    /// row (the `edges` table has no unique constraint) and stale resolutions
    /// (e.g. a confidence that changed because a same-named node elsewhere was
    /// added or removed) are recomputed. `contains` edges are left intact because
    /// the file's nodes are not re-extracted.
    pub fn delete_resolved_edges_from_file(&self, file_path: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            r#"DELETE FROM edges
            WHERE id IN (
              SELECT e.id
              FROM edges e
              JOIN nodes src ON src.id = e.source
              WHERE src.file_path = ?1
                AND e.kind != 'contains'
            )"#,
            [file_path],
        )
    }

    /// Distinct source files of every resolution-produced edge whose TARGET node
    /// is named one of `names`. When a synced file changes the set of nodes
    /// sharing a name, the exact-name resolution of refs that already resolved to
    /// that name (in any file, including the referencing file itself) can change
    /// confidence or pick a different target, but those edges survive untouched.
    /// This finds the files holding such refs so their outgoing resolved edges can
    /// be recomputed. `contains` edges are excluded; results are in SQLite scan
    /// order per chunk.
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
}

fn query_nodes<P>(conn: &Connection, sql: &str, params: P) -> rusqlite::Result<Vec<Node>>
where
    P: rusqlite::Params,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_node)?;
    rows.collect()
}

fn query_edges<P>(conn: &Connection, sql: &str, params: P) -> rusqlite::Result<Vec<Edge>>
where
    P: rusqlite::Params,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_edge)?;
    rows.collect()
}

fn row_to_node(row: &Row<'_>) -> rusqlite::Result<Node> {
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

fn row_to_edge(row: &Row<'_>) -> rusqlite::Result<Edge> {
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

fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        path: row.get("path")?,
        content_hash: row.get("content_hash")?,
        language: parse_language(row.get::<_, String>("language")?)?,
        size: row.get("size")?,
        modified_at: millis_column(row, "modified_at")?,
        indexed_at: millis_column(row, "indexed_at")?,
        node_count: row.get("node_count")?,
        errors: parse_json_array(row.get("errors")?)?,
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

fn row_to_unresolved_ref(row: &Row<'_>) -> rusqlite::Result<UnresolvedRef> {
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
        _ => return Err(enum_error(value, "node kind")),
    };
    Ok(kind)
}

fn parse_edge_kind(value: String) -> rusqlite::Result<EdgeKind> {
    let kind = match value.as_str() {
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
        _ => return Err(enum_error(value, "edge kind")),
    };
    Ok(kind)
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

fn parse_language(value: String) -> rusqlite::Result<Language> {
    let language = match value.as_str() {
        "typescript" => Language::TypeScript,
        "javascript" => Language::JavaScript,
        "tsx" => Language::Tsx,
        "jsx" => Language::Jsx,
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
        }
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

    #[test]
    fn delete_resolved_refs_precise_and_bounded() {
        let mut store = store("delete-resolved");
        store
            .upsert_nodes(&[node("function:src", "src", "a.rs")])
            .unwrap();
        let mk = |name: &str| UnresolvedRef {
            id: None,
            from_node_id: "function:src".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            col: 0,
            candidates: None,
            file_path: "a.rs".to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        };
        store
            .insert_unresolved_refs(&[mk("Alpha"), mk("Beta"), mk("Gamma")])
            .unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 3);

        store.delete_resolved_unresolved_refs(&[]).unwrap();
        store
            .delete_resolved_unresolved_refs_up_to(&[], 100)
            .unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 3);

        store
            .delete_resolved_unresolved_refs(&[(
                "function:src".to_string(),
                "Alpha".to_string(),
                EdgeKind::Calls,
            )])
            .unwrap();
        assert_eq!(store.unresolved_refs_count().unwrap(), 2);

        let rows = store.all_unresolved_refs().unwrap();
        let max_id = rows.iter().map(|r| r.id.unwrap()).min().unwrap();
        store
            .delete_resolved_unresolved_refs_up_to(
                &[(
                    "function:src".to_string(),
                    "Beta".to_string(),
                    EdgeKind::Calls,
                )],
                max_id,
            )
            .unwrap();
        assert!(store.unresolved_refs_count().unwrap() <= 2);
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
