//! The base-graph rows one write connection changes while a log is on
//! (W9-00 row 27, OD-B90).
//!
//! A sync turns the log on before its first write and reads it once its base
//! graph is final, so the synthesis stage knows which rows the sync changed.
//! The log is a TEMP table filled by TEMP triggers on the write connection: an
//! `AFTER INSERT`, `AFTER DELETE` and `AFTER UPDATE` trigger on each of
//! `nodes`, `edges`, `unresolved_refs`, `resolved_refs` and `files` records
//! the row's old image, its new image, or both. TEMP objects live in the
//! connection's temporary schema, never in the main database's
//! `sqlite_master`, and vanish with the connection.
//!
//! Rows compare by content only: a node without `updated_at`, an edge or a
//! reference without its `id`, a file by its path, language and `generated`
//! flag, with its content hash compared on its own (the file's text moved).
//! A synthesized edge is never logged. A row deleted and written back with
//! the same content cancels out, so the log's net change is what the sync
//! moved. Past [`DEFAULT_CHANGE_LOG_CAP`] logged images (or the cap given) the
//! log stops recording and reads as unknown.

use std::collections::BTreeSet;

use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::connection::Store;

/// The logged images past which the log stops and reads as unknown.
pub const DEFAULT_CHANGE_LOG_CAP: usize = 100_000;

/// The TEMP objects, dropped in this order.
const TRIGGERS: &[&str] = &[
    "cg_change_log_nodes_ai",
    "cg_change_log_nodes_ad",
    "cg_change_log_nodes_au",
    "cg_change_log_edges_ai",
    "cg_change_log_edges_ad",
    "cg_change_log_edges_au",
    "cg_change_log_unresolved_ai",
    "cg_change_log_unresolved_ad",
    "cg_change_log_unresolved_au",
    "cg_change_log_resolved_ai",
    "cg_change_log_resolved_ad",
    "cg_change_log_resolved_au",
    "cg_change_log_files_ai",
    "cg_change_log_files_ad",
    "cg_change_log_files_au",
    "cg_change_log_cap",
];

/// The log's table codes.
const NODES: i64 = 0;
const EDGES: i64 = 1;
const UNRESOLVED: i64 = 2;
const RESOLVED: i64 = 3;
const FILES: i64 = 4;
const TEXTS: i64 = 5;

/// A node row's image: every column but `updated_at`.
macro_rules! node_image {
    ($row:literal) => {
        concat!(
            "json_array(",
            $row,
            ".id, ",
            $row,
            ".kind, ",
            $row,
            ".name, ",
            $row,
            ".qualified_name, ",
            $row,
            ".file_path, ",
            $row,
            ".language, ",
            $row,
            ".start_line, ",
            $row,
            ".end_line, ",
            $row,
            ".start_column, ",
            $row,
            ".end_column, ",
            $row,
            ".docstring, ",
            $row,
            ".signature, ",
            $row,
            ".visibility, ",
            $row,
            ".is_exported, ",
            $row,
            ".is_async, ",
            $row,
            ".is_static, ",
            $row,
            ".is_abstract, ",
            $row,
            ".decorators, ",
            $row,
            ".type_parameters, ",
            $row,
            ".return_type)"
        )
    };
}

/// An edge row's image: every column but `id`.
macro_rules! edge_image {
    ($row:literal) => {
        concat!(
            "json_array(",
            $row,
            ".source, ",
            $row,
            ".target, ",
            $row,
            ".kind, ",
            $row,
            ".metadata, ",
            $row,
            ".line, ",
            $row,
            ".col, ",
            $row,
            ".provenance)"
        )
    };
}

/// Whether an edge row is a synthesized one, as the store's `synthesizedBy`
/// predicate reads it (`queries.rs`'s `synthesized_edge!`).
macro_rules! synthesized {
    ($row:literal) => {
        concat!(
            "(CASE WHEN json_valid(",
            $row,
            ".metadata) THEN json_extract(",
            $row,
            ".metadata, '$.synthesizedBy') END IS NOT NULL)"
        )
    };
}

/// A reference row's image (`unresolved_refs` and `resolved_refs` alike):
/// every column but `id`.
macro_rules! ref_image {
    ($row:literal) => {
        concat!(
            "json_array(",
            $row,
            ".from_node_id, ",
            $row,
            ".reference_name, ",
            $row,
            ".reference_kind, ",
            $row,
            ".line, ",
            $row,
            ".col, ",
            $row,
            ".candidates, ",
            $row,
            ".file_path, ",
            $row,
            ".language, ",
            $row,
            ".reference_subkind)"
        )
    };
}

/// A file row's image: its path, language and `generated` flag.
macro_rules! file_image {
    ($row:literal) => {
        concat!(
            "json_array(",
            $row,
            ".path, ",
            $row,
            ".language, ",
            $row,
            ".generated)"
        )
    };
}

/// A file row's text: its path and content hash.
macro_rules! text_image {
    ($row:literal) => {
        concat!("json_array(", $row, ".path, ", $row, ".content_hash)")
    };
}

/// Every trigger records only while the log has not overflowed.
const RECORDING: &str = "(SELECT overflow FROM cg_change_state) = 0";

/// The log's net change: each image's logged count, added minus removed.
const NET: &str = "SELECT tbl, image, SUM(sign) AS net FROM cg_change_log \
                   GROUP BY tbl, image HAVING SUM(sign) != 0 ORDER BY tbl, image";

/// A node row as the log compares it, with the columns the synthesis
/// footprints read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeImage {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub decorators: Option<String>,
}

/// A base edge row as the log compares it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeImage {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub line: Option<i64>,
    pub col: Option<i64>,
}

/// An `unresolved_refs` or `resolved_refs` row as the log compares it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefImage {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    pub line: i64,
    pub col: i64,
    pub file_path: String,
    pub language: String,
}

/// A file row as the log compares it; its text is [`ChangeSet::texts`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileImage {
    pub path: String,
    pub language: String,
    pub generated: bool,
}

/// One table's net change: the images it lost and the ones it gained, each
/// as many times as its net count, in image order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowChanges<T> {
    pub removed: Vec<T>,
    pub added: Vec<T>,
}

impl<T> Default for RowChanges<T> {
    fn default() -> Self {
        Self {
            removed: Vec::new(),
            added: Vec::new(),
        }
    }
}

impl<T> RowChanges<T> {
    /// Every changed image, the removed ones first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.removed.iter().chain(&self.added)
    }

    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }

    fn push(&mut self, image: T, net: i64)
    where
        T: Clone,
    {
        let into = if net < 0 {
            &mut self.removed
        } else {
            &mut self.added
        };
        for _ in 0..net.unsigned_abs() {
            into.push(image.clone());
        }
    }
}

/// What the logged rows moved, by content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub nodes: RowChanges<NodeImage>,
    pub edges: RowChanges<EdgeImage>,
    pub unresolved_refs: RowChanges<RefImage>,
    pub resolved_refs: RowChanges<RefImage>,
    pub files: RowChanges<FileImage>,
    /// The files whose content hash moved, or that came or went, by path.
    pub texts: BTreeSet<String>,
}

impl ChangeSet {
    /// Whether the logged rows moved nothing.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.edges.is_empty()
            && self.unresolved_refs.is_empty()
            && self.resolved_refs.is_empty()
            && self.files.is_empty()
            && self.texts.is_empty()
    }
}

/// `CREATE TEMP TRIGGER` for `table`'s `event`, its body each of `inserts`
/// (`(code, sign, image, guard)`: the log row and the condition it is
/// written under).
fn trigger(
    name: &str,
    event: &str,
    table: &str,
    inserts: &[(i64, i64, &str, Option<&str>)],
) -> String {
    let mut body = String::new();
    for (code, sign, image, guard) in inserts {
        let guard = guard.map_or(String::new(), |guard| format!(" WHERE NOT {guard}"));
        body.push_str(&format!(
            "INSERT INTO cg_change_log (tbl, sign, image) SELECT {code}, {sign}, {image}{guard};\n"
        ));
    }
    format!(
        "CREATE TEMP TRIGGER {name} AFTER {event} ON main.{table} WHEN {RECORDING} BEGIN\n{body}END;\n"
    )
}

/// The statements that turn the log on, `cap` images at most.
fn install_sql(cap: usize) -> String {
    let mut sql = String::from(
        "CREATE TEMP TABLE cg_change_log (\n\
         seq INTEGER PRIMARY KEY,\n\
         tbl INTEGER NOT NULL,\n\
         sign INTEGER NOT NULL,\n\
         image TEXT NOT NULL\n\
         );\n\
         CREATE TEMP TABLE cg_change_state (overflow INTEGER NOT NULL);\n\
         INSERT INTO cg_change_state (overflow) VALUES (0);\n",
    );
    sql.push_str(&trigger(
        "cg_change_log_nodes_ai",
        "INSERT",
        "nodes",
        &[(NODES, 1, node_image!("NEW"), None)],
    ));
    sql.push_str(&trigger(
        "cg_change_log_nodes_ad",
        "DELETE",
        "nodes",
        &[(NODES, -1, node_image!("OLD"), None)],
    ));
    sql.push_str(&trigger(
        "cg_change_log_nodes_au",
        "UPDATE",
        "nodes",
        &[
            (NODES, -1, node_image!("OLD"), None),
            (NODES, 1, node_image!("NEW"), None),
        ],
    ));
    sql.push_str(&trigger(
        "cg_change_log_edges_ai",
        "INSERT",
        "edges",
        &[(EDGES, 1, edge_image!("NEW"), Some(synthesized!("NEW")))],
    ));
    sql.push_str(&trigger(
        "cg_change_log_edges_ad",
        "DELETE",
        "edges",
        &[(EDGES, -1, edge_image!("OLD"), Some(synthesized!("OLD")))],
    ));
    sql.push_str(&trigger(
        "cg_change_log_edges_au",
        "UPDATE",
        "edges",
        &[
            (EDGES, -1, edge_image!("OLD"), Some(synthesized!("OLD"))),
            (EDGES, 1, edge_image!("NEW"), Some(synthesized!("NEW"))),
        ],
    ));
    for (prefix, table, code) in [
        ("unresolved", "unresolved_refs", UNRESOLVED),
        ("resolved", "resolved_refs", RESOLVED),
    ] {
        sql.push_str(&trigger(
            &format!("cg_change_log_{prefix}_ai"),
            "INSERT",
            table,
            &[(code, 1, ref_image!("NEW"), None)],
        ));
        sql.push_str(&trigger(
            &format!("cg_change_log_{prefix}_ad"),
            "DELETE",
            table,
            &[(code, -1, ref_image!("OLD"), None)],
        ));
        sql.push_str(&trigger(
            &format!("cg_change_log_{prefix}_au"),
            "UPDATE",
            table,
            &[
                (code, -1, ref_image!("OLD"), None),
                (code, 1, ref_image!("NEW"), None),
            ],
        ));
    }
    sql.push_str(&trigger(
        "cg_change_log_files_ai",
        "INSERT",
        "files",
        &[
            (FILES, 1, file_image!("NEW"), None),
            (TEXTS, 1, text_image!("NEW"), None),
        ],
    ));
    sql.push_str(&trigger(
        "cg_change_log_files_ad",
        "DELETE",
        "files",
        &[
            (FILES, -1, file_image!("OLD"), None),
            (TEXTS, -1, text_image!("OLD"), None),
        ],
    ));
    sql.push_str(&trigger(
        "cg_change_log_files_au",
        "UPDATE",
        "files",
        &[
            (FILES, -1, file_image!("OLD"), None),
            (FILES, 1, file_image!("NEW"), None),
            (TEXTS, -1, text_image!("OLD"), None),
            (TEXTS, 1, text_image!("NEW"), None),
        ],
    ));
    // Past the cap the log stops and drops what it holds: it reads as
    // unknown, and the memory goes.
    sql.push_str(&format!(
        "CREATE TEMP TRIGGER cg_change_log_cap AFTER INSERT ON cg_change_log WHEN NEW.seq > {cap} BEGIN\n\
         UPDATE cg_change_state SET overflow = 1;\n\
         DELETE FROM cg_change_log;\n\
         END;\n"
    ));
    sql
}

/// The statements that turn the log off.
fn remove_sql() -> String {
    let mut sql = String::new();
    for name in TRIGGERS {
        sql.push_str(&format!("DROP TRIGGER IF EXISTS temp.{name};\n"));
    }
    sql.push_str("DROP TABLE IF EXISTS temp.cg_change_log;\n");
    sql.push_str("DROP TABLE IF EXISTS temp.cg_change_state;\n");
    sql
}

fn text_at(image: &[Value], at: usize) -> String {
    match image.get(at) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn optional_text_at(image: &[Value], at: usize) -> Option<String> {
    match image.get(at) {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn integer_at(image: &[Value], at: usize) -> Option<i64> {
    image.get(at).and_then(Value::as_i64)
}

fn bad_image(code: i64, image: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        2,
        rusqlite::types::Type::Text,
        format!("change log image of table {code} is not a JSON array: {image}").into(),
    )
}

impl Store {
    /// Turn the change log on for this connection's writes, `cap` logged
    /// images at most, replacing a log already on. Recursive triggers are on
    /// while it is, so a row an `INSERT OR REPLACE` replaces is logged as
    /// deleted.
    pub fn begin_change_log(&self, cap: usize) -> rusqlite::Result<()> {
        self.conn.execute_batch(&remove_sql())?;
        self.conn.execute_batch(&install_sql(cap))?;
        self.conn.pragma_update(None, "recursive_triggers", "ON")
    }

    /// Whether the change log is on for this connection.
    pub fn change_log_active(&self) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM temp.sqlite_master WHERE type = 'table' AND name = 'cg_change_state')",
            [],
            |row| row.get(0),
        )
    }

    /// Turn the change log off and forget it. A log that is off stays off.
    pub fn end_change_log(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(&remove_sql())?;
        self.conn.pragma_update(None, "recursive_triggers", "OFF")
    }

    /// The change log's net change, and the log turned off: `None` when it
    /// was not on or overflowed, so what changed is unknown.
    pub fn take_change_log(&self) -> rusqlite::Result<Option<ChangeSet>> {
        if !self.change_log_active()? {
            return Ok(None);
        }
        let overflowed: Option<i64> = self
            .conn
            .query_row("SELECT overflow FROM temp.cg_change_state", [], |row| {
                row.get(0)
            })
            .optional()?;
        let changes = if overflowed == Some(0) {
            Some(self.read_change_log()?)
        } else {
            None
        };
        self.end_change_log()?;
        Ok(changes)
    }

    fn read_change_log(&self) -> rusqlite::Result<ChangeSet> {
        let mut changes = ChangeSet::default();
        let mut stmt = self.conn.prepare(NET)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let code: i64 = row.get(0)?;
            let raw: String = row.get(1)?;
            let net: i64 = row.get(2)?;
            let image: Vec<Value> =
                serde_json::from_str(&raw).map_err(|_| bad_image(code, &raw))?;
            match code {
                NODES => changes.nodes.push(
                    NodeImage {
                        id: text_at(&image, 0),
                        kind: text_at(&image, 1),
                        name: text_at(&image, 2),
                        qualified_name: text_at(&image, 3),
                        file_path: text_at(&image, 4),
                        language: text_at(&image, 5),
                        start_line: integer_at(&image, 6).unwrap_or_default(),
                        end_line: integer_at(&image, 7).unwrap_or_default(),
                        signature: optional_text_at(&image, 11),
                        decorators: optional_text_at(&image, 17),
                    },
                    net,
                ),
                EDGES => changes.edges.push(
                    EdgeImage {
                        source: text_at(&image, 0),
                        target: text_at(&image, 1),
                        kind: text_at(&image, 2),
                        line: integer_at(&image, 4),
                        col: integer_at(&image, 5),
                    },
                    net,
                ),
                UNRESOLVED | RESOLVED => {
                    let reference = RefImage {
                        from_node_id: text_at(&image, 0),
                        reference_name: text_at(&image, 1),
                        reference_kind: text_at(&image, 2),
                        line: integer_at(&image, 3).unwrap_or_default(),
                        col: integer_at(&image, 4).unwrap_or_default(),
                        file_path: text_at(&image, 6),
                        language: text_at(&image, 7),
                    };
                    if code == UNRESOLVED {
                        changes.unresolved_refs.push(reference, net);
                    } else {
                        changes.resolved_refs.push(reference, net);
                    }
                }
                FILES => changes.files.push(
                    FileImage {
                        path: text_at(&image, 0),
                        language: text_at(&image, 1),
                        generated: integer_at(&image, 2).unwrap_or_default() != 0,
                    },
                    net,
                ),
                TEXTS => {
                    changes.texts.insert(text_at(&image, 0));
                }
                _ => return Err(bad_image(code, &raw)),
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind};
    use serde_json::json;

    fn store(name: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "cg-change-log-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir.join("index.db")).unwrap(), dir)
    }

    fn node(id: &str, name: &str, file: &str, line: i64) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: line,
            end_line: line + 2,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: Some(format!("function {name}()")),
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 1,
        }
    }

    fn edge(source: &str, target: &str, line: i64) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: Some(line),
            col: Some(0),
            provenance: None,
        }
    }

    fn file(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: hash.to_string(),
            language: Language::TypeScript,
            size: 1,
            modified_at: 1,
            indexed_at: 1,
            node_count: 0,
            errors: Vec::new(),
            generated: false,
        }
    }

    fn reference(store: &Store, from: &str, name: &str, line: i64) {
        store
            .connection()
            .execute(
                "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language) \
                 VALUES (?1, ?2, 'calls', ?3, 0, 'src/a.ts', 'typescript')",
                rusqlite::params![from, name, line],
            )
            .unwrap();
    }

    fn ids(changes: &RowChanges<NodeImage>) -> (Vec<&str>, Vec<&str>) {
        (
            changes
                .removed
                .iter()
                .map(|node| node.id.as_str())
                .collect(),
            changes.added.iter().map(|node| node.id.as_str()).collect(),
        )
    }

    fn main_schema(store: &Store) -> Vec<(String, String)> {
        let mut stmt = store
            .connection()
            .prepare("SELECT type, name FROM main.sqlite_master ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Inserts, deletes and in-place updates of each base table are logged
    /// with their content; a log that is not on reads as unknown.
    #[test]
    fn the_log_records_inserts_deletes_and_updates() {
        let (mut store, dir) = store("basic");
        assert_eq!(store.take_change_log().unwrap(), None, "not on");
        store
            .upsert_nodes(&[node("f:a", "a", "src/a.ts", 1)])
            .unwrap();
        store.upsert_file(&file("src/a.ts", "h1")).unwrap();
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        assert!(store.change_log_active().unwrap());
        store
            .upsert_nodes(&[node("f:b", "b", "src/a.ts", 5)])
            .unwrap();
        store.insert_edges(&[edge("f:a", "f:b", 2)]).unwrap();
        reference(&store, "f:a", "c", 3);
        store
            .connection()
            .execute(
                "UPDATE nodes SET end_line = 9, updated_at = 2 WHERE id = 'f:a'",
                [],
            )
            .unwrap();
        store.upsert_file(&file("src/a.ts", "h2")).unwrap();
        let changes = store.take_change_log().unwrap().expect("a known change");
        assert!(!store.change_log_active().unwrap(), "taking turns it off");
        let (removed, added) = ids(&changes.nodes);
        assert_eq!(removed, ["f:a"]);
        assert_eq!(added, ["f:a", "f:b"]);
        assert_eq!(changes.nodes.removed[0].end_line, 3);
        let updated = changes
            .nodes
            .added
            .iter()
            .find(|node| node.id == "f:a")
            .unwrap();
        assert_eq!(updated.end_line, 9);
        assert_eq!(updated.signature.as_deref(), Some("function a()"));
        assert_eq!(
            changes.edges.added,
            [EdgeImage {
                source: "f:a".to_string(),
                target: "f:b".to_string(),
                kind: "calls".to_string(),
                line: Some(2),
                col: Some(0),
            }]
        );
        assert_eq!(changes.unresolved_refs.added.len(), 1);
        assert_eq!(changes.unresolved_refs.added[0].reference_name, "c");
        assert!(changes.files.is_empty(), "path, language and flag kept");
        assert_eq!(
            changes.texts.iter().collect::<Vec<_>>(),
            ["src/a.ts"],
            "its text moved"
        );
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Deleting a node logs the edges and references its foreign keys
    /// cascade to; a synthesized edge is never logged.
    #[test]
    fn a_cascade_is_logged_and_a_synthesized_edge_is_not() {
        let (mut store, dir) = store("cascade");
        store
            .upsert_nodes(&[
                node("f:a", "a", "src/a.ts", 1),
                node("f:b", "b", "src/b.ts", 1),
            ])
            .unwrap();
        let mut synthesized = edge("f:b", "f:a", 7);
        synthesized.metadata = Some(json!({ "synthesizedBy": "callback" }));
        store
            .insert_edges(&[edge("f:a", "f:b", 2), synthesized])
            .unwrap();
        reference(&store, "f:a", "x", 4);
        store
            .connection()
            .execute(
                "INSERT INTO resolved_refs (id, from_node_id, reference_name, reference_kind, line, col, file_path, language) \
                 VALUES (90, 'f:a', 'y', 'calls', 5, 1, 'src/a.ts', 'typescript')",
                [],
            )
            .unwrap();
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        store
            .connection()
            .execute("DELETE FROM nodes WHERE id = 'f:a'", [])
            .unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert_eq!(ids(&changes.nodes), (vec!["f:a"], vec![]));
        assert_eq!(changes.edges.removed.len(), 1, "{:?}", changes.edges);
        assert_eq!(changes.edges.removed[0].line, Some(2), "the base edge");
        assert_eq!(changes.unresolved_refs.removed.len(), 1);
        assert_eq!(changes.resolved_refs.removed.len(), 1);
        assert_eq!(changes.resolved_refs.removed[0].reference_name, "y");
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    /// An upsert over an existing row logs its old and new images; a row
    /// deleted and written back the same cancels out, `updated_at` and ids
    /// aside; a row an `INSERT OR REPLACE` replaces is logged as deleted.
    #[test]
    fn upserts_net_out_and_a_replace_logs_the_replaced_row() {
        let (mut store, dir) = store("net");
        let a = node("f:a", "a", "src/a.ts", 1);
        store.upsert_nodes(std::slice::from_ref(&a)).unwrap();
        store.insert_edges(&[edge("f:a", "f:a", 2)]).unwrap();
        reference(&store, "f:a", "x", 4);
        store.upsert_file(&file("src/a.ts", "h1")).unwrap();
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        // Written back the same: a later timestamp, a new edge and ref id.
        let again = Node {
            updated_at: 99,
            ..a.clone()
        };
        store.upsert_nodes(std::slice::from_ref(&again)).unwrap();
        store.connection().execute("DELETE FROM edges", []).unwrap();
        store.insert_edges(&[edge("f:a", "f:a", 2)]).unwrap();
        store
            .connection()
            .execute("DELETE FROM unresolved_refs", [])
            .unwrap();
        reference(&store, "f:a", "x", 4);
        store.upsert_file(&file("src/a.ts", "h1")).unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert!(changes.is_empty(), "{changes:?}");

        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        let moved = Node {
            name: "renamed".to_string(),
            ..a.clone()
        };
        store.upsert_nodes(std::slice::from_ref(&moved)).unwrap();
        // A hash that goes and comes back is no text change.
        store.upsert_file(&file("src/a.ts", "h2")).unwrap();
        store.upsert_file(&file("src/a.ts", "h1")).unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert_eq!(changes.nodes.removed[0].name, "a");
        assert_eq!(changes.nodes.added[0].name, "renamed");
        assert!(changes.texts.is_empty());

        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        store
            .connection()
            .execute(
                "INSERT OR REPLACE INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors, generated) \
                 VALUES ('src/a.ts', 'h3', 'javascript', 1, 1, 1, 0, NULL, 1)",
                [],
            )
            .unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert_eq!(
            changes.files.removed,
            [FileImage {
                path: "src/a.ts".to_string(),
                language: "typescript".to_string(),
                generated: false,
            }],
            "the replaced row"
        );
        assert_eq!(changes.files.added[0].language, "javascript");
        assert!(changes.files.added[0].generated);
        assert!(changes.texts.contains("src/a.ts"));
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Past its cap the log stops, drops what it held and reads as unknown;
    /// turning it off stops the recording.
    #[test]
    fn an_overflowing_log_reads_as_unknown_and_an_ended_one_records_nothing() {
        let (mut store, dir) = store("overflow");
        store.begin_change_log(3).unwrap();
        store
            .upsert_nodes(&[
                node("f:a", "a", "src/a.ts", 1),
                node("f:b", "b", "src/a.ts", 5),
                node("f:c", "c", "src/a.ts", 9),
            ])
            .unwrap();
        let held: i64 = store
            .connection()
            .query_row("SELECT count(*) FROM temp.cg_change_log", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(held, 3, "at the cap");
        store
            .upsert_nodes(&[node("f:d", "d", "src/a.ts", 13)])
            .unwrap();
        let held: i64 = store
            .connection()
            .query_row("SELECT count(*) FROM temp.cg_change_log", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(held, 0, "past it the log drops what it held");
        assert_eq!(store.take_change_log().unwrap(), None, "overflowed");

        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        store.end_change_log().unwrap();
        assert!(!store.change_log_active().unwrap());
        store
            .upsert_nodes(&[node("f:e", "e", "src/a.ts", 17)])
            .unwrap();
        assert_eq!(store.take_change_log().unwrap(), None, "off");
        let recursive: i64 = store
            .connection()
            .query_row("PRAGMA recursive_triggers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(recursive, 0, "restored");
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    /// The log's objects live in the temporary schema: the main database's
    /// `sqlite_master` is the same with the log on, after it and after an
    /// overflow; a statement prepared and cached before the log went on
    /// writes through its triggers, and one cached while it was on stops
    /// logging once it is off.
    #[test]
    fn the_main_schema_and_cached_statements_are_unaffected() {
        let (store, dir) = store("schema");
        let before = main_schema(&store);
        let insert = "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, \
                      start_line, end_line, start_column, end_column, updated_at) \
                      VALUES (?1, 'function', ?1, ?1, 'src/a.ts', 'typescript', 1, 1, 0, 0, 0)";
        store
            .connection()
            .prepare_cached(insert)
            .unwrap()
            .execute(["f:warm"])
            .unwrap();
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        assert_eq!(main_schema(&store), before);
        store
            .connection()
            .prepare_cached(insert)
            .unwrap()
            .execute(["f:a"])
            .unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert_eq!(ids(&changes.nodes), (vec![], vec!["f:a"]));
        assert_eq!(main_schema(&store), before);

        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        store
            .connection()
            .prepare_cached(insert)
            .unwrap()
            .execute(["f:b"])
            .unwrap();
        store.end_change_log().unwrap();
        store
            .connection()
            .prepare_cached(insert)
            .unwrap()
            .execute(["f:c"])
            .unwrap();
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        let changes = store.take_change_log().unwrap().unwrap();
        assert!(changes.is_empty(), "f:c was written while it was off");
        assert_eq!(main_schema(&store), before);
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A rolled-back write leaves nothing in the log.
    #[test]
    fn a_rolled_back_write_is_not_logged() {
        let (mut store, dir) = store("rollback");
        store.begin_change_log(DEFAULT_CHANGE_LOG_CAP).unwrap();
        {
            let tx = store.conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, \
                 start_line, end_line, start_column, end_column, updated_at) \
                 VALUES ('f:a', 'function', 'a', 'a', 'src/a.ts', 'typescript', 1, 1, 0, 0, 0)",
                [],
            )
            .unwrap();
            tx.rollback().unwrap();
        }
        assert_eq!(store.take_change_log().unwrap(), Some(ChangeSet::default()));
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }
}
