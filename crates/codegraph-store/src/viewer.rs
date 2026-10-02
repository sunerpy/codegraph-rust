//! Read-only aggregate queries the browser viewer (`codegraph-ui`) answers from.
//!
//! Each is a port of the upstream query the viewer's server calls
//! (`src/db/queries.ts`, `v1.6.1`), named after it. All of them are SELECTs: the
//! viewer never writes the index.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};
use rusqlite::{ToSql, params};

use crate::connection::Store;
use crate::queries::{SQLITE_PARAM_CHUNK_SIZE, row_to_edge, row_to_node, row_to_unresolved_ref};

/// One executable root, as `getTopCallingFiles` ranks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopCallingFile {
    pub node_id: String,
    pub file_path: String,
    pub calls: i64,
    pub reaches: i64,
    pub score: i64,
}

/// One route → handler row, as `getRoutingManifest` selects it (before the
/// test/generated filter, which needs file classification the caller owns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRow {
    pub url: String,
    pub route_id: String,
    pub route_file: String,
    pub route_line: i64,
    pub handler: String,
    pub handler_file: String,
    pub handler_line: i64,
    pub handler_kind: String,
}

/// Cross-file edge counts by file pair, kind and symbol names — the input the
/// module map folds into module links (upstream `aggregateModuleGraph`'s single
/// pass, grouped at file rather than module level so no temp table is needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePairEdgeRow {
    pub source_file: String,
    pub target_file: String,
    pub kind: EdgeKind,
    pub from_name: String,
    pub to_name: String,
    /// Edges at or above the confidence floor.
    pub count: i64,
    /// Of those, the ones resolved by declaration (import / qualified name /
    /// extends / implements / confident instance-method).
    pub declared: i64,
    /// Edges below the confidence floor.
    pub uncertain: i64,
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn unique(ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// The last segment of a (possibly qualified) reference name — what a plain
/// node name could match: `util.greet` → `greet`, `mod::fn` → `fn`, and for an
/// Erlang ref with a written arity (`mod::fn/2`) the arity-less name.
pub fn reference_name_tail(reference_name: &str) -> &str {
    let base = match reference_name.rsplit_once('/') {
        Some((head, arity))
            if !head.is_empty()
                && (1..=3).contains(&arity.len())
                && arity.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => reference_name,
    };
    match base.rfind(['.', ':']) {
        Some(at) => &base[at + 1..],
        None => base,
    }
}

impl Store {
    /// `getStats`' edge histogram: `SELECT kind, COUNT(*) FROM edges GROUP BY kind`.
    pub fn edge_counts_by_kind(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self
            .connection()
            .prepare("SELECT kind, COUNT(*) FROM edges GROUP BY kind ORDER BY kind")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// `getLastIndexedAt`: the most recent `indexed_at` across files.
    pub fn last_indexed_at(&self) -> rusqlite::Result<Option<i64>> {
        self.connection()
            .query_row("SELECT MAX(indexed_at) FROM files", [], |row| {
                row.get::<_, Option<f64>>(0)
            })
            .map(|v| v.map(|f| f as i64))
    }

    /// `getIndexRevision`: `(MAX(indexed_at), COUNT(*))` over files — moves on a
    /// sync that only deletes, which `MAX` alone would miss.
    pub fn index_revision(&self) -> rusqlite::Result<(Option<i64>, i64)> {
        self.connection()
            .query_row("SELECT MAX(indexed_at), COUNT(*) FROM files", [], |row| {
                Ok((row.get::<_, Option<f64>>(0)?.map(|f| f as i64), row.get(1)?))
            })
    }

    /// `getFilesIndexedSince`: files re-indexed strictly after `since`, newest
    /// first, capped at `limit`, with the true total.
    pub fn files_indexed_since(
        &self,
        since: i64,
        limit: i64,
    ) -> rusqlite::Result<(Vec<String>, i64)> {
        let total: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM files WHERE indexed_at > ?",
            [since],
            |row| row.get(0),
        )?;
        let mut stmt = self.connection().prepare(
            "SELECT path FROM files WHERE indexed_at > ? ORDER BY indexed_at DESC, path LIMIT ?",
        )?;
        let rows = stmt.query_map(params![since, limit.max(0)], |row| row.get::<_, String>(0))?;
        Ok((rows.collect::<rusqlite::Result<Vec<_>>>()?, total))
    }

    /// `countGeneratedFiles`: files flagged tool-generated.
    pub fn generated_file_count(&self) -> rusqlite::Result<i64> {
        self.connection().query_row(
            "SELECT COUNT(*) FROM files WHERE generated = 1",
            [],
            |row| row.get(0),
        )
    }

    /// `PRAGMA journal_mode`.
    pub fn journal_mode(&self) -> rusqlite::Result<String> {
        self.connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
    }

    fn batched_edges(
        &self,
        column: &str,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        let ids = unique(ids);
        let mut out = Vec::new();
        for chunk in ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let mut sql = format!(
                "SELECT * FROM edges WHERE {column} IN ({})",
                placeholders(chunk.len())
            );
            let mut values: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
            let kind_strs: Vec<&'static str> = kinds.iter().map(|k| k.as_str()).collect();
            if !kind_strs.is_empty() {
                sql.push_str(&format!(" AND kind IN ({})", placeholders(kind_strs.len())));
                values.extend(kind_strs.iter().map(|k| k as &dyn ToSql));
            }
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), row_to_edge)?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// `getOutgoingEdgesFrom`: every edge out of any of `ids`, optionally only
    /// of `kinds`, one batched query per chunk.
    pub fn outgoing_edges_from(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        self.batched_edges("source", ids, kinds)
    }

    /// `getIncomingEdgesTo`: the mirror of [`Self::outgoing_edges_from`].
    pub fn incoming_edges_to(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        self.batched_edges("target", ids, kinds)
    }

    fn batched_counts(
        &self,
        column: &str,
        ids: &[String],
    ) -> rusqlite::Result<HashMap<String, i64>> {
        let ids = unique(ids);
        let mut out = HashMap::new();
        for chunk in ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT {column}, COUNT(*) FROM edges WHERE {column} IN ({}) GROUP BY {column}",
                placeholders(chunk.len())
            );
            let values: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (id, count) = row?;
                out.insert(id, count);
            }
        }
        Ok(out)
    }

    /// `countIncomingEdges` (`getFanIn`): incoming edge count per id.
    pub fn count_incoming_edges(&self, ids: &[String]) -> rusqlite::Result<HashMap<String, i64>> {
        self.batched_counts("target", ids)
    }

    /// `countOutgoingEdges` (`getFanOut`): outgoing edge count per id.
    pub fn count_outgoing_edges(&self, ids: &[String]) -> rusqlite::Result<HashMap<String, i64>> {
        self.batched_counts("source", ids)
    }

    /// `getTopDependedOn`: the nodes with the most distinct dependents.
    pub fn top_depended_on(&self, limit: i64) -> rusqlite::Result<Vec<(String, i64)>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.connection().prepare(
            "SELECT target, COUNT(DISTINCT source) AS dependents
               FROM edges
              WHERE kind != 'contains' AND source != target
           GROUP BY target
           ORDER BY dependents DESC, target
              LIMIT ?",
        )?;
        let rows = stmt.query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// `getTopCallingFiles`: files that run something at module level, ranked by
    /// calls × (1 + distinct other files their symbols reach).
    pub fn top_calling_files(&self, limit: i64) -> rusqlite::Result<Vec<TopCallingFile>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.connection().prepare(
            "WITH tops AS (
                 SELECT n.id AS file_id, n.id AS src
                   FROM nodes n
                  WHERE n.kind = 'file'
                 UNION ALL
                 SELECT c.source AS file_id, c.target AS src
                   FROM edges c
                   JOIN nodes f ON f.id = c.source
                   JOIN nodes v ON v.id = c.target
                  WHERE c.kind = 'contains'
                    AND f.kind = 'file'
                    AND v.kind IN ('variable', 'constant')
             ),
             runs AS (
                 SELECT t.file_id AS id, COUNT(*) AS calls
                   FROM tops t
                   JOIN edges e ON e.source = t.src
                  WHERE e.kind IN ('calls', 'instantiates')
               GROUP BY t.file_id
             ),
             cand AS (
                 SELECT r.id AS id, n.file_path AS fp, r.calls AS calls
                   FROM runs r JOIN nodes n ON n.id = r.id
             ),
             wires AS (
                 SELECT sn.file_path AS fp, COUNT(DISTINCT tn.file_path) AS reaches
                   FROM edges e
                   JOIN nodes sn ON sn.id = e.source
                   JOIN nodes tn ON tn.id = e.target
                  WHERE e.kind != 'contains'
                    AND sn.file_path <> tn.file_path
                    AND sn.file_path IN (SELECT fp FROM cand)
               GROUP BY sn.file_path
             )
             SELECT c.id, c.fp, c.calls, COALESCE(w.reaches, 0),
                    c.calls * (1 + COALESCE(w.reaches, 0)) AS score
               FROM cand c LEFT JOIN wires w ON w.fp = c.fp
           ORDER BY score DESC, c.calls DESC, c.fp
              LIMIT ?",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(TopCallingFile {
                node_id: row.get(0)?,
                file_path: row.get(1)?,
                calls: row.get(2)?,
                reaches: row.get(3)?,
                score: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// `getFileDependentCounts`: distinct other files depending on each file.
    pub fn file_dependent_counts(&self, paths: &[String]) -> rusqlite::Result<Vec<(String, i64)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_string());
        let mut stmt = self.connection().prepare(
            "SELECT tn.file_path, COUNT(DISTINCT sn.file_path)
               FROM edges e
               JOIN nodes tn ON tn.id = e.target
               JOIN nodes sn ON sn.id = e.source
              WHERE e.kind != 'contains'
                AND tn.file_path IN (SELECT value FROM json_each(?))
                AND sn.file_path <> tn.file_path
           GROUP BY tn.file_path
           ORDER BY tn.file_path",
        )?;
        let rows = stmt.query_map([json], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// `getFileReachCounts`: per file, the distinct other files its symbols reach
    /// and the number of such references.
    pub fn file_reach_counts(&self, paths: &[String]) -> rusqlite::Result<Vec<(String, i64, i64)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_string());
        let mut stmt = self.connection().prepare(
            "SELECT sn.file_path, COUNT(DISTINCT tn.file_path), COUNT(*)
               FROM nodes sn
               JOIN edges e ON e.source = sn.id
               JOIN nodes tn ON tn.id = e.target
              WHERE sn.file_path IN (SELECT value FROM json_each(?))
                AND e.kind != 'contains'
                AND tn.file_path <> sn.file_path
           GROUP BY sn.file_path
           ORDER BY sn.file_path",
        )?;
        let rows = stmt.query_map([json], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// `getFileNodes`: the `file` nodes of `paths`.
    pub fn file_nodes(&self, paths: &[String]) -> rusqlite::Result<Vec<Node>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_string());
        let mut stmt = self.connection().prepare(
            "SELECT * FROM nodes WHERE kind = 'file' AND file_path IN (SELECT value FROM json_each(?)) ORDER BY file_path",
        )?;
        let rows = stmt.query_map([json], row_to_node)?;
        rows.collect()
    }

    /// `getNodesByFiles`: every node in any of `paths`, one chunked `IN` query
    /// per chunk rather than one query per file (#1975). Ordered by file, line
    /// and id, so a caller that keys these by location resolves ties the same
    /// way on every run.
    pub fn nodes_in_files(&self, paths: &[String]) -> rusqlite::Result<Vec<Node>> {
        let paths = unique(paths);
        let mut out = Vec::new();
        for chunk in paths.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM nodes WHERE file_path IN ({}) ORDER BY file_path, start_line, id",
                placeholders(chunk.len())
            );
            let values: Vec<&dyn ToSql> = chunk.iter().map(|p| p as &dyn ToSql).collect();
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), row_to_node)?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// `getStats().nodesByKind[kind]`: how many nodes of one kind the index holds.
    pub fn count_nodes_of_kind(&self, kind: NodeKind) -> rusqlite::Result<i64> {
        self.connection().query_row(
            "SELECT COUNT(*) FROM nodes WHERE kind = ?",
            [kind.as_str()],
            |row| row.get(0),
        )
    }

    /// `getNodesByNamePrefix`: nodes whose name starts with `prefix`, by index
    /// range scan (a LIKE would skip `idx_nodes_name`).
    pub fn nodes_by_name_prefix(&self, prefix: &str, limit: i64) -> rusqlite::Result<Vec<Node>> {
        let upper = format!("{prefix}\u{ffff}");
        let mut stmt = self.connection().prepare(
            "SELECT * FROM nodes WHERE name >= ? AND name < ? ORDER BY name, id LIMIT ?",
        )?;
        let rows = stmt.query_map(params![prefix, upper, limit], row_to_node)?;
        rows.collect()
    }

    /// `getNodesByKind`, in upstream's canonical order.
    pub fn nodes_of_kind_ordered(&self, kind: NodeKind) -> rusqlite::Result<Vec<Node>> {
        let mut stmt = self
            .connection()
            .prepare("SELECT * FROM nodes WHERE kind = ? ORDER BY file_path, start_line, id")?;
        let rows = stmt.query_map([kind.as_str()], row_to_node)?;
        rows.collect()
    }

    /// `getRoutingManifest`'s SELECT: route → handler rows, routes in file and
    /// line order, at most `limit`.
    pub fn routing_manifest_rows(&self, limit: i64) -> rusqlite::Result<Vec<RoutingRow>> {
        let mut stmt = self.connection().prepare(
            "SELECT r.name, r.id, r.file_path, r.start_line,
                    h.name, h.file_path, h.start_line, h.kind
               FROM nodes r
               JOIN edges e ON e.source = r.id
               JOIN nodes h ON e.target = h.id
              WHERE r.kind = 'route'
                AND e.kind IN ('references', 'calls')
                AND h.kind IN ('function', 'method', 'class', 'constant', 'variable')
           ORDER BY r.file_path, r.start_line, r.id, h.id
              LIMIT ?",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(RoutingRow {
                url: row.get(0)?,
                route_id: row.get(1)?,
                route_file: row.get(2)?,
                route_line: row.get(3)?,
                handler: row.get(4)?,
                handler_file: row.get(5)?,
                handler_line: row.get(6)?,
                handler_kind: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// The module map's single pass, grouped by FILE pair, kind and the two
    /// symbol names (upstream groups by module pair; the caller maps files to
    /// modules and folds, which is the same sum without a temp table).
    pub fn cross_file_edge_rows(
        &self,
        kinds: &[EdgeKind],
        min_confidence: f64,
    ) -> rusqlite::Result<Vec<FilePairEdgeRow>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let confidence = "COALESCE(json_extract(e.metadata, '$.confidence'), 1)";
        let declared = format!(
            "(json_extract(e.metadata, '$.resolvedBy') IN ('import', 'qualified-name') \
              OR e.kind IN ('extends', 'implements') \
              OR (json_extract(e.metadata, '$.resolvedBy') = 'instance-method' AND {confidence} >= 0.9))"
        );
        let sql = format!(
            "SELECT sn.file_path, tn.file_path, e.kind, sn.name, tn.name,
                    SUM(CASE WHEN {confidence} >= ?1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN {confidence} >= ?1 AND {declared} THEN 1 ELSE 0 END),
                    SUM(CASE WHEN {confidence} <  ?1 THEN 1 ELSE 0 END)
               FROM edges e
               JOIN nodes sn ON sn.id = e.source
               JOIN nodes tn ON tn.id = e.target
              WHERE e.kind IN (SELECT value FROM json_each(?2))
                AND sn.file_path <> tn.file_path
           GROUP BY sn.file_path, tn.file_path, e.kind, sn.name, tn.name
           ORDER BY sn.file_path, tn.file_path, e.kind, sn.name, tn.name"
        );
        let kinds_json =
            serde_json::to_string(&kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>())
                .unwrap_or_else(|_| "[]".to_string());
        let mut stmt = self.connection().prepare(&sql)?;
        let rows = stmt.query_map(params![min_confidence, kinds_json], |row| {
            let kind: String = row.get(2)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                kind,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (source_file, target_file, kind, from_name, to_name, count, declared, uncertain) =
                row?;
            let Some(kind) = EdgeKind::ALL.into_iter().find(|k| k.as_str() == kind) else {
                continue;
            };
            out.push(FilePairEdgeRow {
                source_file,
                target_file,
                kind,
                from_name,
                to_name,
                count,
                declared,
                uncertain,
            });
        }
        Ok(out)
    }

    /// `getCrossFileDependencyPairs`: every ordered pair of distinct files where
    /// one reaches into the other at or above `min_confidence`.
    pub fn cross_file_dependency_pairs(
        &self,
        min_confidence: f64,
    ) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.connection().prepare(
            "SELECT DISTINCT sn.file_path, tn.file_path
               FROM edges e
               JOIN nodes sn ON sn.id = e.source
               JOIN nodes tn ON tn.id = e.target
              WHERE e.kind <> 'contains'
                AND sn.file_path <> tn.file_path
                AND COALESCE(json_extract(e.metadata, '$.confidence'), 1) >= ?
           ORDER BY sn.file_path, tn.file_path",
        )?;
        let rows = stmt.query_map([min_confidence], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// `getUnreferencedNodes`: symbols of `kinds` that no non-`contains` edge
    /// points at, with their file's generated flag — the dead-code candidates.
    pub fn unreferenced_nodes(
        &self,
        kinds: &[NodeKind],
        limit: i64,
    ) -> rusqlite::Result<Vec<(Node, bool)>> {
        if kinds.is_empty() || limit <= 0 {
            return Ok(Vec::new());
        }
        let kinds_json =
            serde_json::to_string(&kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>())
                .unwrap_or_else(|_| "[]".to_string());
        let mut stmt = self.connection().prepare(
            "SELECT n.*, COALESCE(f.generated, 0) AS file_generated
               FROM nodes n
               LEFT JOIN files f ON f.path = n.file_path
              WHERE n.kind IN (SELECT value FROM json_each(?1))
                AND NOT EXISTS (
                      SELECT 1 FROM edges e
                       WHERE e.target = n.id AND e.kind != 'contains' AND e.source != n.id
                    )
           ORDER BY n.file_path, n.start_line, n.name, n.id
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![kinds_json, limit], |row| {
            Ok((row_to_node(row)?, row.get::<_, i64>("file_generated")? != 0))
        })?;
        rows.collect()
    }

    /// `getUnresolvedNamesAmong`: which of `names` the index holds an
    /// unresolved reference to, matched on the reference name AND on its tail
    /// (`util.greet` → `greet`, `mod::fn` → `fn`, Erlang `f/1` → `f`), because
    /// the question is "could this be the name we failed to follow", and a maybe
    /// counts as a yes. One pass over the distinct reference names (the schema
    /// carries no tail column, and the name index covers the scan).
    pub fn unresolved_names_among(&self, names: &[String]) -> rusqlite::Result<HashSet<String>> {
        let wanted: HashSet<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| !n.is_empty())
            .collect();
        let mut found = HashSet::new();
        if wanted.is_empty() {
            return Ok(found);
        }
        let mut stmt = self
            .connection()
            .prepare("SELECT DISTINCT reference_name FROM unresolved_refs")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let name = row?;
            if wanted.contains(name.as_str()) {
                found.insert(name.clone());
            }
            let tail = reference_name_tail(&name);
            if wanted.contains(tail) {
                found.insert(tail.to_string());
            }
        }
        Ok(found)
    }

    /// `getUnresolvedSupertypeSourcesAmong`: which of `ids` extend or implement
    /// a type the resolver could not follow — the one record that an ancestor
    /// outside the index exists (#1973).
    pub fn unresolved_supertype_sources_among(
        &self,
        ids: &[String],
    ) -> rusqlite::Result<HashSet<String>> {
        let ids = unique(ids);
        let mut found = HashSet::new();
        for chunk in ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT DISTINCT from_node_id FROM unresolved_refs
                  WHERE from_node_id IN ({})
                    AND reference_kind IN ('extends', 'implements')",
                placeholders(chunk.len())
            );
            let values: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), |row| row.get::<_, String>(0))?;
            for row in rows {
                found.insert(row?);
            }
        }
        Ok(found)
    }

    /// `getAmbiguousReferencedNames`: which of `names` are carried by MORE THAN
    /// ONE symbol, at least one of which something points at (self-edges
    /// counted — a self-edge is the fingerprint of a same-file mis-resolution).
    pub fn ambiguous_referenced_names(
        &self,
        names: &[String],
    ) -> rusqlite::Result<HashSet<String>> {
        let names: Vec<String> = unique(names)
            .into_iter()
            .filter(|n| !n.is_empty())
            .collect();
        let mut found = HashSet::new();
        for chunk in names.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT name FROM (
                     SELECT n.name AS name,
                            EXISTS (SELECT 1 FROM edges e WHERE e.target = n.id AND e.kind != 'contains') AS referenced
                       FROM nodes n
                      WHERE n.name IN ({})
                 )
               GROUP BY name
                 HAVING COUNT(*) > 1 AND SUM(referenced) > 0",
                placeholders(chunk.len())
            );
            let values: Vec<&dyn ToSql> = chunk.iter().map(|n| n as &dyn ToSql).collect();
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), |row| row.get::<_, String>(0))?;
            for row in rows {
                found.insert(row?);
            }
        }
        Ok(found)
    }

    /// `getLanguagesWithExports`: which of `languages` the index records an
    /// export marker for. A language with none (Rust `pub` is not recorded;
    /// Python and C have no such concept) is one the exported filter cannot run
    /// on, and the caller must say so rather than claim outside-unreachability.
    pub fn languages_with_exports(
        &self,
        languages: &[String],
    ) -> rusqlite::Result<HashSet<String>> {
        let languages: Vec<String> = unique(languages)
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect();
        let mut found = HashSet::new();
        for chunk in languages.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT language, MAX(is_exported) FROM nodes WHERE language IN ({}) GROUP BY language",
                placeholders(chunk.len())
            );
            let values: Vec<&dyn ToSql> = chunk.iter().map(|l| l as &dyn ToSql).collect();
            let mut stmt = self.connection().prepare(&sql)?;
            let rows = stmt.query_map(values.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
            })?;
            for row in rows {
                let (language, any) = row?;
                if any == Some(1) {
                    found.insert(language);
                }
            }
        }
        Ok(found)
    }

    /// `getUnresolvedReferencesFrom`: references from one symbol that never
    /// resolved to an indexed node, in line order.
    pub fn unresolved_refs_from(&self, from_node_id: &str) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.connection().prepare(
            "SELECT * FROM unresolved_refs WHERE from_node_id = ? ORDER BY line, col, id",
        )?;
        let rows = stmt.query_map([from_node_id], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// `getUnresolvedReferencesInFile`: every unresolved reference in a file, in
    /// line order, capped.
    pub fn unresolved_refs_in_file(
        &self,
        file_path: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<UnresolvedRef>> {
        let mut stmt = self.connection().prepare(
            "SELECT * FROM unresolved_refs WHERE file_path = ? ORDER BY line, col, id LIMIT ?",
        )?;
        let rows = stmt.query_map(params![file_path, limit], row_to_unresolved_ref)?;
        rows.collect()
    }

    /// `findNodesByNameSubstring`: nodes whose name contains `substring` (SQLite
    /// LIKE, ASCII-case-insensitive), shortest name first.
    pub fn nodes_by_name_substring(
        &self,
        substring: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<Node>> {
        let mut stmt = self.connection().prepare(
            "SELECT * FROM nodes WHERE name LIKE ? ORDER BY length(name) ASC, name, file_path, start_line, id LIMIT ?",
        )?;
        let rows = stmt.query_map(params![format!("%{substring}%"), limit], row_to_node)?;
        rows.collect()
    }
}
