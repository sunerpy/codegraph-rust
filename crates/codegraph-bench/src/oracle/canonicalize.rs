use std::collections::BTreeMap;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub type CanonicalRow = BTreeMap<String, Value>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanonicalDb {
    pub nodes: Vec<CanonicalRow>,
    pub edges: Vec<CanonicalRow>,
    pub unresolved_refs: Vec<CanonicalRow>,
    pub files: Vec<CanonicalRow>,
    pub schema: String,
}

pub fn canonicalize_db(db_path: &Path) -> Result<CanonicalDb> {
    let conn =
        Connection::open(db_path).with_context(|| format!("opening {}", db_path.display()))?;
    Ok(CanonicalDb {
        nodes: canonical_nodes(&conn)?,
        edges: canonical_edges(&conn)?,
        unresolved_refs: canonical_refs(&conn)?,
        files: canonical_files(&conn)?,
        schema: sqlite_schema(db_path)?,
    })
}

pub fn normalize_schema(schema: &str) -> String {
    let mut normalized = schema
        .split(';')
        .map(normalize_statement)
        .filter(|statement| !statement.is_empty())
        .collect::<Vec<_>>()
        .join(";\n");
    normalized.push_str(";\n");
    normalized
}

fn normalize_statement(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("CREATE TABLE IF NOT EXISTS ", "CREATE TABLE ")
        .replace("CREATE INDEX IF NOT EXISTS ", "CREATE INDEX ")
        .replace(
            "CREATE VIRTUAL TABLE IF NOT EXISTS ",
            "CREATE VIRTUAL TABLE ",
        )
        .replace("CREATE TRIGGER IF NOT EXISTS ", "CREATE TRIGGER ")
}

fn canonical_nodes(conn: &Connection) -> Result<Vec<CanonicalRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, qualified_name, file_path, language, start_line, end_line, \
         start_column, end_column, docstring, signature, visibility, is_exported, is_async, \
         is_static, is_abstract, decorators, type_parameters, return_type FROM nodes",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RawNode {
            id: row.get(0)?,
            kind: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            file_path: row.get(4)?,
            language: row.get(5)?,
            start_line: row.get(6)?,
            end_line: row.get(7)?,
            start_column: row.get(8)?,
            end_column: row.get(9)?,
            docstring: row.get(10)?,
            signature: row.get(11)?,
            visibility: row.get(12)?,
            is_exported: row.get(13)?,
            is_async: row.get(14)?,
            is_static: row.get(15)?,
            is_abstract: row.get(16)?,
            decorators: row.get(17)?,
            type_parameters: row.get(18)?,
            return_type: row.get(19)?,
        })
    })?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?.canonical()?);
    }
    nodes.sort_by(|left, right| row_key(left, "id").cmp(row_key(right, "id")));
    Ok(nodes)
}

fn canonical_edges(conn: &Connection) -> Result<Vec<CanonicalRow>> {
    let mut stmt =
        conn.prepare("SELECT source, target, kind, metadata, line, col, provenance FROM edges")?;
    let rows = stmt.query_map([], |row| {
        Ok(RawEdge {
            source: row.get(0)?,
            target: row.get(1)?,
            kind: row.get(2)?,
            metadata: row.get(3)?,
            line: row.get(4)?,
            col: row.get(5)?,
            provenance: row.get(6)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?.canonical()?);
    }
    sort_rows_by_json(&mut edges);
    Ok(edges)
}

fn canonical_refs(conn: &Connection) -> Result<Vec<CanonicalRow>> {
    let mut stmt = conn.prepare(
        "SELECT from_node_id, reference_name, reference_kind, line, col, candidates, file_path, \
         language, reference_subkind FROM unresolved_refs",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RawRef {
            from_node_id: row.get(0)?,
            reference_name: row.get(1)?,
            reference_kind: row.get(2)?,
            line: row.get(3)?,
            col: row.get(4)?,
            candidates: row.get(5)?,
            file_path: row.get(6)?,
            language: row.get(7)?,
            reference_subkind: row.get(8)?,
        })
    })?;

    let mut refs = Vec::new();
    for row in rows {
        refs.push(row?.canonical()?);
    }
    sort_rows_by_json(&mut refs);
    Ok(refs)
}

fn canonical_files(conn: &Connection) -> Result<Vec<CanonicalRow>> {
    let mut stmt =
        conn.prepare("SELECT path, content_hash, language, size, node_count, errors FROM files")?;
    let rows = stmt.query_map([], |row| {
        Ok(RawFile {
            path: row.get(0)?,
            content_hash: row.get(1)?,
            language: row.get(2)?,
            size: row.get(3)?,
            node_count: row.get(4)?,
            errors: row.get(5)?,
        })
    })?;

    let mut files = Vec::new();
    for row in rows {
        files.push(row?.canonical()?);
    }
    files.sort_by(|left, right| row_key(left, "path").cmp(row_key(right, "path")));
    Ok(files)
}

fn sqlite_schema(db_path: &Path) -> Result<String> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening {} for schema dump", db_path.display()))?;
    Ok(normalize_schema(&schema_dump(&conn)?))
}

// Byte-equivalent to `sqlite3 .schema`: rowid order, and the `/* name(cols) */`
// comment the shell appends after each CREATE VIRTUAL TABLE (kept so the golden
// schema string matches the old CLI path).
fn schema_dump(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT name, type, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut raw = String::new();
    for (name, kind, sql) in rows {
        raw.push_str(&sql);
        if kind == "table" && sql.starts_with("CREATE VIRTUAL TABLE") {
            raw.push_str(&format!(
                "\n/* {}({}) */",
                name,
                virtual_table_columns(conn, &name)?
            ));
        }
        raw.push_str(";\n");
    }
    Ok(raw)
}

fn virtual_table_columns(conn: &Connection, table: &str) -> Result<String> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let columns = stmt
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns.join(","))
}

fn assert_relative_slash_path(path: &str, column: &str) -> Result<()> {
    if path.is_empty() {
        bail!("{column} path is empty");
    }
    if path.contains('\\') {
        bail!("{column} must use '/' separators: {path}");
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        bail!("{column} must be a relative path: {path}");
    }
    Ok(())
}

fn json_column(value: Option<String>, column: &str) -> Result<Value> {
    match value {
        Some(text) => serde_json::from_str(&text)
            .with_context(|| format!("parsing JSON column {column}: {text}")),
        None => Ok(Value::Null),
    }
}

fn optional_string(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

fn row_key<'a>(row: &'a CanonicalRow, column: &str) -> &'a str {
    row.get(column).and_then(Value::as_str).unwrap_or("")
}

fn sort_rows_by_json(rows: &mut [CanonicalRow]) {
    rows.sort_by(|left, right| {
        let left_json = serde_json::to_string(left).expect("canonical row serializes");
        let right_json = serde_json::to_string(right).expect("canonical row serializes");
        left_json.cmp(&right_json)
    });
}

struct RawNode {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file_path: String,
    language: String,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<String>,
    is_exported: i64,
    is_async: i64,
    is_static: i64,
    is_abstract: i64,
    decorators: Option<String>,
    type_parameters: Option<String>,
    return_type: Option<String>,
}

impl RawNode {
    fn canonical(self) -> Result<CanonicalRow> {
        assert_relative_slash_path(&self.file_path, "nodes.file_path")?;
        let mut row = CanonicalRow::new();
        row.insert("id".to_string(), json!(self.id));
        row.insert("kind".to_string(), json!(self.kind));
        row.insert("name".to_string(), json!(self.name));
        row.insert("qualified_name".to_string(), json!(self.qualified_name));
        row.insert("file_path".to_string(), json!(self.file_path));
        row.insert("language".to_string(), json!(self.language));
        row.insert("start_line".to_string(), json!(self.start_line));
        row.insert("end_line".to_string(), json!(self.end_line));
        row.insert("start_column".to_string(), json!(self.start_column));
        row.insert("end_column".to_string(), json!(self.end_column));
        row.insert("docstring".to_string(), optional_string(self.docstring));
        row.insert("signature".to_string(), optional_string(self.signature));
        row.insert("visibility".to_string(), optional_string(self.visibility));
        row.insert("is_exported".to_string(), json!(self.is_exported));
        row.insert("is_async".to_string(), json!(self.is_async));
        row.insert("is_static".to_string(), json!(self.is_static));
        row.insert("is_abstract".to_string(), json!(self.is_abstract));
        row.insert(
            "decorators".to_string(),
            json_column(self.decorators, "decorators")?,
        );
        row.insert(
            "type_parameters".to_string(),
            json_column(self.type_parameters, "type_parameters")?,
        );
        row.insert("return_type".to_string(), optional_string(self.return_type));
        Ok(row)
    }
}

struct RawEdge {
    source: String,
    target: String,
    kind: String,
    metadata: Option<String>,
    line: Option<i64>,
    col: Option<i64>,
    provenance: Option<String>,
}

impl RawEdge {
    fn canonical(self) -> Result<CanonicalRow> {
        let mut row = CanonicalRow::new();
        row.insert("source".to_string(), json!(self.source));
        row.insert("target".to_string(), json!(self.target));
        row.insert("kind".to_string(), json!(self.kind));
        row.insert(
            "metadata".to_string(),
            json_column(self.metadata, "metadata")?,
        );
        row.insert(
            "line".to_string(),
            self.line.map_or(Value::Null, |v| json!(v)),
        );
        row.insert(
            "col".to_string(),
            self.col.map_or(Value::Null, |v| json!(v)),
        );
        row.insert("provenance".to_string(), optional_string(self.provenance));
        Ok(row)
    }
}

struct RawRef {
    from_node_id: String,
    reference_name: String,
    reference_kind: String,
    line: i64,
    col: i64,
    candidates: Option<String>,
    file_path: String,
    language: String,
    reference_subkind: Option<String>,
}

impl RawRef {
    fn canonical(self) -> Result<CanonicalRow> {
        assert_relative_slash_path(&self.file_path, "unresolved_refs.file_path")?;
        let mut row = CanonicalRow::new();
        row.insert("from_node_id".to_string(), json!(self.from_node_id));
        row.insert("reference_name".to_string(), json!(self.reference_name));
        row.insert("reference_kind".to_string(), json!(self.reference_kind));
        row.insert("line".to_string(), json!(self.line));
        row.insert("col".to_string(), json!(self.col));
        row.insert(
            "candidates".to_string(),
            json_column(self.candidates, "candidates")?,
        );
        row.insert("file_path".to_string(), json!(self.file_path));
        row.insert("language".to_string(), json!(self.language));
        // RULE G1: omit the key entirely when NULL so every existing (non-Godot)
        // row stays byte-identical; only Godot rows that carry a subkind differ.
        if let Some(subkind) = self.reference_subkind {
            row.insert("reference_subkind".to_string(), json!(subkind));
        }
        Ok(row)
    }
}

struct RawFile {
    path: String,
    content_hash: String,
    language: String,
    size: i64,
    node_count: i64,
    errors: Option<String>,
}

impl RawFile {
    fn canonical(self) -> Result<CanonicalRow> {
        assert_relative_slash_path(&self.path, "files.path")?;
        let mut row = CanonicalRow::new();
        row.insert("path".to_string(), json!(self.path));
        row.insert("content_hash".to_string(), json!(self.content_hash));
        row.insert("language".to_string(), json!(self.language));
        row.insert("size".to_string(), json!(self.size));
        row.insert("node_count".to_string(), json!(self.node_count));
        row.insert("errors".to_string(), json_column(self.errors, "errors")?);
        Ok(row)
    }
}
