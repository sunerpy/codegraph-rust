//! `bench --graph-diff`: compare two canonical graphs and print what changed.
//!
//! Either input is a CodeGraph SQLite database or a golden directory
//! (`nodes.json`, `edges.json`, `refs.json`, `files.json`, `schema.sql`). Both
//! go through the oracle canonicalization, so a database and the golden written
//! from it compare identical.
//!
//! A database is copied, together with its `-wal` when one exists, into a
//! private temporary directory and opened only there. SQLite therefore never
//! creates `-wal`/`-shm` sidecars beside the input, which would make the
//! CodeGraph CLI refuse a published index. The copy must pass
//! `PRAGMA quick_check` before it is read, so a corrupted file stops the
//! comparison instead of yielding a partial graph.
//!
//! Rows are compared as multisets of canonical JSON. A row only on the left is
//! removed and a row only on the right is added, so a node or file whose
//! attributes changed shows as one removed and one added row with the same key.
//! Changes are grouped by (surface, language, kind, resolvedBy) and every list
//! is sorted, so identical inputs always produce byte-identical output.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::oracle::{CanonicalDb, CanonicalRow, canonicalize_db, load_golden};

/// Placeholder for a group column a surface does not have.
const NONE: &str = "-";
/// Language of an edge whose source node is in neither graph.
const UNKNOWN_LANGUAGE: &str = "?";

/// The surfaces a [`GraphFingerprint`] hashes, in the order the combined hash
/// reads them.
pub const FINGERPRINT_SURFACES: [&str; 5] =
    ["nodes", "edges", "unresolved_refs", "files", "schema"];

/// The row surfaces of a canonical graph, in report order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Surface {
    Nodes,
    Edges,
    UnresolvedRefs,
    Files,
}

impl Surface {
    pub const ALL: [Self; 4] = [Self::Nodes, Self::Edges, Self::UnresolvedRefs, Self::Files];

    pub fn name(self) -> &'static str {
        match self {
            Self::Nodes => "nodes",
            Self::Edges => "edges",
            Self::UnresolvedRefs => "unresolved_refs",
            Self::Files => "files",
        }
    }

    fn rows(self, graph: &CanonicalDb) -> &[CanonicalRow] {
        match self {
            Self::Nodes => &graph.nodes,
            Self::Edges => &graph.edges,
            Self::UnresolvedRefs => &graph.unresolved_refs,
            Self::Files => &graph.files,
        }
    }
}

/// What kind of input a graph was loaded from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphSource {
    Database,
    Golden,
}

impl GraphSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Golden => "golden",
        }
    }
}

/// Load a canonical graph from a golden directory or a SQLite database file.
pub fn load_graph(path: &Path) -> Result<(GraphSource, CanonicalDb)> {
    let metadata = fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if metadata.is_dir() {
        let graph = load_golden(path)
            .with_context(|| format!("cannot load the golden directory {}", path.display()))?;
        return Ok((GraphSource::Golden, graph));
    }
    if !metadata.is_file() {
        bail!(
            "{} is neither a database file nor a golden directory",
            path.display()
        );
    }
    let scratch = ScratchDir::new("graph-diff")?;
    let copy = scratch.path().join("graph.db");
    fs::copy(path, &copy)
        .with_context(|| format!("copying {} to {}", path.display(), copy.display()))?;
    let wal = with_suffix(path, "-wal");
    if wal.is_file() {
        let wal_copy = with_suffix(&copy, "-wal");
        fs::copy(&wal, &wal_copy)
            .with_context(|| format!("copying {} to {}", wal.display(), wal_copy.display()))?;
    }
    let graph = canonicalize_checked(&copy)
        .with_context(|| format!("cannot canonicalize the database {}", path.display()))?;
    Ok((GraphSource::Database, graph))
}

/// Canonicalize the database at `db` after it passes `PRAGMA quick_check`.
///
/// SQLite may write `-wal`/`-shm` sidecars next to `db`, so callers pass a file
/// they own: a scratch copy, or the index of a scratch project.
pub fn canonicalize_checked(db: &Path) -> Result<CanonicalDb> {
    if !db.is_file() {
        bail!("{} is not a database file", db.display());
    }
    {
        let conn = Connection::open(db).with_context(|| format!("opening {}", db.display()))?;
        let mut statement = conn
            .prepare("PRAGMA quick_check")
            .with_context(|| format!("running PRAGMA quick_check on {}", db.display()))?;
        let verdict = statement
            .query_map([], |row| row.get::<_, String>(0))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .with_context(|| format!("running PRAGMA quick_check on {}", db.display()))?;
        if verdict != ["ok"] {
            bail!(
                "PRAGMA quick_check failed for {}: {}",
                db.display(),
                verdict.join("; ")
            );
        }
    }
    canonicalize_db(db)
}

/// Rows per surface of a canonical graph.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RowCounts {
    pub nodes: u64,
    pub edges: u64,
    pub unresolved_refs: u64,
    pub files: u64,
    pub total: u64,
}

impl RowCounts {
    pub fn of(graph: &CanonicalDb) -> Self {
        let count = |rows: &[CanonicalRow]| rows.len() as u64;
        let nodes = count(&graph.nodes);
        let edges = count(&graph.edges);
        let unresolved_refs = count(&graph.unresolved_refs);
        let files = count(&graph.files);
        Self {
            nodes,
            edges,
            unresolved_refs,
            files,
            total: nodes + edges + unresolved_refs + files,
        }
    }
}

/// SHA-256 identity of a canonical graph.
///
/// Each row surface hashes the compact JSON array of its canonical rows;
/// `schema` hashes the normalized schema text. `hash` is the SHA-256 of the
/// lines `"<surface> <sha256>\n"` in [`FINGERPRINT_SURFACES`] order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphFingerprint {
    pub hash: String,
    pub surfaces: BTreeMap<String, String>,
    pub rows: RowCounts,
}

pub fn fingerprint(graph: &CanonicalDb) -> GraphFingerprint {
    let mut surfaces = BTreeMap::new();
    for surface in Surface::ALL {
        surfaces.insert(surface.name().to_string(), hash_rows(surface.rows(graph)));
    }
    surfaces.insert(
        "schema".to_string(),
        hex(&Sha256::digest(graph.schema.as_bytes())),
    );
    let mut combined = Sha256::new();
    for name in FINGERPRINT_SURFACES {
        combined.update(format!("{name} {}\n", surfaces[name]).as_bytes());
    }
    GraphFingerprint {
        hash: hex(&combined.finalize()),
        surfaces,
        rows: RowCounts::of(graph),
    }
}

/// The [`FINGERPRINT_SURFACES`] whose hashes differ between two fingerprints.
pub fn differing_surfaces(left: &GraphFingerprint, right: &GraphFingerprint) -> Vec<String> {
    FINGERPRINT_SURFACES
        .iter()
        .filter(|name| left.surfaces.get(**name) != right.surfaces.get(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// Lowercase hex SHA-256 of a file's bytes.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// One (surface, language, kind, resolvedBy) group of changed rows.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GroupKey {
    pub surface: Surface,
    pub language: String,
    pub kind: String,
    pub resolved_by: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupDiff {
    pub key: GroupKey,
    /// Canonical JSON of rows only in the left graph, sorted.
    pub removed: Vec<String>,
    /// Canonical JSON of rows only in the right graph, sorted.
    pub added: Vec<String>,
}

/// Schema lines that are only on one side.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemaDiff {
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphDiff {
    /// Changed groups in (surface, language, kind, resolvedBy) order.
    pub groups: Vec<GroupDiff>,
    /// `Some` when the normalized schemas differ.
    pub schema: Option<SchemaDiff>,
}

impl GraphDiff {
    pub fn is_identical(&self) -> bool {
        self.groups.is_empty() && self.schema.is_none()
    }

    /// (removed, added) row counts of one surface.
    pub fn totals(&self, surface: Surface) -> (usize, usize) {
        self.groups
            .iter()
            .filter(|group| group.key.surface == surface)
            .fold((0, 0), |(removed, added), group| {
                (removed + group.removed.len(), added + group.added.len())
            })
    }
}

/// Compare two canonical graphs row by row.
///
/// The group of a node is its own language and kind, of a reference its
/// language and `reference_kind`, and of a file its language. An edge takes the
/// language of its source node (looked up on its own side first, then on the
/// other) and its `metadata.resolvedBy`; a synthesized edge without one shows
/// `synthesizedBy:<metadata.synthesizedBy>`.
pub fn diff_graphs(left: &CanonicalDb, right: &CanonicalDb) -> GraphDiff {
    let left_languages = node_languages(left);
    let right_languages = node_languages(right);
    let mut groups: BTreeMap<GroupKey, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for surface in Surface::ALL {
        let left_rows = counted_rows(surface.rows(left));
        let right_rows = counted_rows(surface.rows(right));
        for (json, row) in surplus_rows(&left_rows, &right_rows) {
            let key = group_key(surface, row, &left_languages, &right_languages);
            groups.entry(key).or_default().0.push(json);
        }
        for (json, row) in surplus_rows(&right_rows, &left_rows) {
            let key = group_key(surface, row, &right_languages, &left_languages);
            groups.entry(key).or_default().1.push(json);
        }
    }
    let groups = groups
        .into_iter()
        .map(|(key, (mut removed, mut added))| {
            removed.sort();
            added.sort();
            GroupDiff {
                key,
                removed,
                added,
            }
        })
        .collect();
    let schema = (left.schema != right.schema).then(|| SchemaDiff {
        removed: unmatched_lines(&left.schema, &right.schema),
        added: unmatched_lines(&right.schema, &left.schema),
    });
    GraphDiff { groups, schema }
}

/// Load both inputs, write the report to `out`, and return whether the graphs
/// are identical. Nothing is written when an input cannot be loaded.
pub fn run_graph_diff(left: &Path, right: &Path, out: &mut dyn Write) -> Result<bool> {
    let (left_source, left_graph) = load_graph(left)?;
    let (right_source, right_graph) = load_graph(right)?;
    let diff = diff_graphs(&left_graph, &right_graph);
    let report = render_diff(
        &format!("{} ({})", left.display(), left_source.name()),
        &format!("{} ({})", right.display(), right_source.name()),
        &diff,
    );
    out.write_all(report.as_bytes())
        .context("writing the graph-diff report")?;
    out.flush().context("writing the graph-diff report")?;
    Ok(diff.is_identical())
}

/// Render a [`GraphDiff`] as the plain-text `bench --graph-diff` report.
pub fn render_diff(left: &str, right: &str, diff: &GraphDiff) -> String {
    let mut out = format!("left:  {left}\nright: {right}\n\n");
    out.push_str("surface          removed    added\n");
    let mut removed_total = 0;
    let mut added_total = 0;
    for surface in Surface::ALL {
        let (removed, added) = diff.totals(surface);
        removed_total += removed;
        added_total += added;
        out.push_str(&format!("{:<15} {removed:>8} {added:>8}\n", surface.name()));
    }
    let schema_state = if diff.schema.is_some() {
        "differs"
    } else {
        "identical"
    };
    out.push_str(&format!("schema          {schema_state}\n"));

    if !diff.groups.is_empty() {
        out.push_str("\ngroups:\n");
        out.push_str(&group_table(&diff.groups));
        out.push_str("\nrows:\n");
        for group in &diff.groups {
            let key = &group.key;
            out.push_str(&format!(
                "@@ {} {} {} {} (-{} +{})\n",
                key.surface.name(),
                key.language,
                key.kind,
                key.resolved_by,
                group.removed.len(),
                group.added.len()
            ));
            for row in &group.removed {
                out.push_str(&format!("- {row}\n"));
            }
            for row in &group.added {
                out.push_str(&format!("+ {row}\n"));
            }
        }
    }
    if let Some(schema) = &diff.schema {
        out.push_str("\n@@ schema\n");
        if schema.removed.is_empty() && schema.added.is_empty() {
            out.push_str("(the same lines in a different order)\n");
        }
        for line in &schema.removed {
            out.push_str(&format!("- {line}\n"));
        }
        for line in &schema.added {
            out.push_str(&format!("+ {line}\n"));
        }
    }

    if diff.is_identical() {
        out.push_str("\nresult: identical\n");
    } else {
        out.push_str(&format!(
            "\nresult: different ({removed_total} removed, {added_total} added rows; schema {schema_state})\n"
        ));
    }
    out
}

fn group_table(groups: &[GroupDiff]) -> String {
    let header = ["surface", "language", "kind", "resolvedBy"];
    let cells = |key: &GroupKey| -> [String; 4] {
        [
            key.surface.name().to_string(),
            key.language.clone(),
            key.kind.clone(),
            key.resolved_by.clone(),
        ]
    };
    let mut widths = header.map(str::len);
    for group in groups {
        for (width, value) in widths.iter_mut().zip(cells(&group.key)) {
            *width = (*width).max(value.len());
        }
    }
    let line = |cells: [&str; 4], removed: &str, added: &str| {
        format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {removed:>8} {added:>8}\n",
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        )
    };
    let mut out = line(header, "removed", "added");
    for group in groups {
        let values = cells(&group.key);
        out.push_str(&line(
            values.each_ref().map(String::as_str),
            &group.removed.len().to_string(),
            &group.added.len().to_string(),
        ));
    }
    out
}

/// Each distinct canonical row of a surface with its multiplicity, keyed (and
/// therefore ordered) by its compact JSON.
fn counted_rows(rows: &[CanonicalRow]) -> BTreeMap<String, (usize, &CanonicalRow)> {
    let mut counted: BTreeMap<String, (usize, &CanonicalRow)> = BTreeMap::new();
    for row in rows {
        counted.entry(row_json(row)).or_insert((0, row)).0 += 1;
    }
    counted
}

/// Rows of `from` that `other` does not match, repeated per unmatched copy.
fn surplus_rows<'a>(
    from: &BTreeMap<String, (usize, &'a CanonicalRow)>,
    other: &BTreeMap<String, (usize, &'a CanonicalRow)>,
) -> Vec<(String, &'a CanonicalRow)> {
    let mut rows = Vec::new();
    for (json, (count, row)) in from {
        let matched = other.get(json).map_or(0, |(other_count, _)| *other_count);
        for _ in matched..*count {
            rows.push((json.clone(), *row));
        }
    }
    rows
}

fn node_languages(graph: &CanonicalDb) -> HashMap<&str, &str> {
    graph
        .nodes
        .iter()
        .filter_map(|node| {
            let id = node.get("id")?.as_str()?;
            let language = node.get("language")?.as_str()?;
            Some((id, language))
        })
        .collect()
}

fn group_key(
    surface: Surface,
    row: &CanonicalRow,
    own_languages: &HashMap<&str, &str>,
    other_languages: &HashMap<&str, &str>,
) -> GroupKey {
    let (language, kind, resolved_by) = match surface {
        Surface::Nodes => (column(row, "language"), column(row, "kind"), NONE.into()),
        Surface::Edges => {
            let source = row.get("source").and_then(Value::as_str).unwrap_or("");
            let language = own_languages
                .get(source)
                .or_else(|| other_languages.get(source))
                .map_or(UNKNOWN_LANGUAGE, |language| *language);
            (
                language.to_string(),
                column(row, "kind"),
                edge_resolution(row),
            )
        }
        Surface::UnresolvedRefs => (
            column(row, "language"),
            column(row, "reference_kind"),
            NONE.into(),
        ),
        Surface::Files => (column(row, "language"), "file".into(), NONE.into()),
    };
    GroupKey {
        surface,
        language,
        kind,
        resolved_by,
    }
}

fn column(row: &CanonicalRow, name: &str) -> String {
    match row.get(name) {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) => NONE.to_string(),
        Some(other) => other.to_string(),
    }
}

fn edge_resolution(row: &CanonicalRow) -> String {
    let Some(metadata) = row.get("metadata").and_then(Value::as_object) else {
        return NONE.to_string();
    };
    if let Some(resolved_by) = metadata.get("resolvedBy").and_then(Value::as_str) {
        return resolved_by.to_string();
    }
    if let Some(synthesized_by) = metadata.get("synthesizedBy").and_then(Value::as_str) {
        return format!("synthesizedBy:{synthesized_by}");
    }
    NONE.to_string()
}

/// Lines of `from` left over after matching each against one equal line of
/// `other`, in `from` order.
fn unmatched_lines(from: &str, other: &str) -> Vec<String> {
    let mut available: HashMap<&str, usize> = HashMap::new();
    for line in other.lines() {
        *available.entry(line).or_insert(0) += 1;
    }
    let mut unmatched = Vec::new();
    for line in from.lines() {
        match available.get_mut(line) {
            Some(count) if *count > 0 => *count -= 1,
            _ => unmatched.push(line.to_string()),
        }
    }
    unmatched
}

fn row_json(row: &CanonicalRow) -> String {
    serde_json::to_string(row).expect("canonical rows serialize")
}

fn hash_rows(rows: &[CanonicalRow]) -> String {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, rows).expect("canonical rows serialize");
    hex(&writer.0.finalize())
}

/// Feeds everything written to it into a SHA-256 hasher.
struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A uniquely named directory under the system temp dir, removed on drop.
pub(crate) struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub(crate) fn new(label: &str) -> Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir();
        for _ in 0..64 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "codegraph-bench-{label}-{}-{nanos}-{serial}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("creating {}", path.display()));
                }
            }
        }
        bail!(
            "cannot create a unique scratch directory under {}",
            base.display()
        )
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(value: Value) -> CanonicalRow {
        serde_json::from_value(value).expect("object row")
    }

    fn graph(nodes: Vec<Value>, edges: Vec<Value>) -> CanonicalDb {
        CanonicalDb {
            nodes: nodes.into_iter().map(row).collect(),
            edges: edges.into_iter().map(row).collect(),
            unresolved_refs: Vec::new(),
            files: Vec::new(),
            schema: "CREATE TABLE nodes (id TEXT);\n".to_string(),
        }
    }

    fn node(id: &str, language: &str, name: &str) -> Value {
        json!({"id": id, "kind": "function", "language": language, "name": name})
    }

    fn edge(source: &str, target: &str, resolved_by: Option<&str>) -> Value {
        let metadata = resolved_by.map_or(Value::Null, |by| json!({"resolvedBy": by}));
        json!({"source": source, "target": target, "kind": "calls", "metadata": metadata})
    }

    #[test]
    fn identical_graphs_have_no_groups_and_identical_fingerprints() {
        let left = graph(
            vec![node("function:a", "go", "a")],
            vec![edge("function:a", "function:a", None)],
        );
        let diff = diff_graphs(&left, &left.clone());
        assert!(diff.is_identical());
        assert_eq!(fingerprint(&left), fingerprint(&left.clone()));
        assert!(render_diff("l", "r", &diff).ends_with("\nresult: identical\n"));
    }

    #[test]
    fn rows_group_by_surface_language_kind_and_resolution() {
        let left = graph(
            vec![
                node("function:a", "go", "a"),
                node("function:b", "rust", "b"),
            ],
            vec![
                edge("function:a", "function:b", Some("exact-match")),
                edge("function:b", "function:a", Some("import")),
            ],
        );
        let right = graph(
            vec![
                node("function:a", "go", "a"),
                node("function:b", "rust", "b_renamed"),
            ],
            vec![
                edge("function:b", "function:a", Some("import")),
                edge("function:b", "function:a", Some("import")),
                json!({"source": "function:zz", "target": "function:a", "kind": "calls",
                       "metadata": {"synthesizedBy": "callback"}}),
            ],
        );
        let diff = diff_graphs(&left, &right);
        let keys: Vec<(&str, &str, &str, &str, usize, usize)> = diff
            .groups
            .iter()
            .map(|group| {
                (
                    group.key.surface.name(),
                    group.key.language.as_str(),
                    group.key.kind.as_str(),
                    group.key.resolved_by.as_str(),
                    group.removed.len(),
                    group.added.len(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                ("nodes", "rust", "function", "-", 1, 1),
                ("edges", "?", "calls", "synthesizedBy:callback", 0, 1),
                ("edges", "go", "calls", "exact-match", 1, 0),
                // The duplicate import edge is one surplus copy, not two.
                ("edges", "rust", "calls", "import", 0, 1),
            ]
        );
        assert_eq!(diff.totals(Surface::Edges), (1, 2));
        assert!(diff.schema.is_none());
        let fingerprints = (fingerprint(&left), fingerprint(&right));
        assert_eq!(
            differing_surfaces(&fingerprints.0, &fingerprints.1),
            vec!["nodes".to_string(), "edges".to_string()]
        );
        assert_ne!(fingerprints.0.hash, fingerprints.1.hash);
        assert_eq!(fingerprints.1.rows.edges, 3);
        assert_eq!(fingerprints.1.rows.total, 5);
    }

    #[test]
    fn rendering_is_deterministic_and_lists_every_row() {
        let left = graph(vec![node("function:a", "go", "a")], Vec::new());
        let mut right = graph(Vec::new(), Vec::new());
        right.schema = "CREATE TABLE nodes (id TEXT);\nCREATE INDEX idx ON nodes(id);\n".into();
        let diff = diff_graphs(&left, &right);
        let first = render_diff("left.db (database)", "golden (golden)", &diff);
        assert_eq!(
            first,
            render_diff("left.db (database)", "golden (golden)", &diff)
        );
        assert!(first.contains("\n@@ nodes go function - (-1 +0)\n- {"));
        assert!(first.contains("\n@@ schema\n+ CREATE INDEX idx ON nodes(id);\n"));
        assert!(first.ends_with("\nresult: different (1 removed, 0 added rows; schema differs)\n"));
    }

    #[test]
    fn reordered_schema_lines_are_reported_as_a_difference() {
        let mut left = graph(Vec::new(), Vec::new());
        let mut right = left.clone();
        left.schema = "A;\nB;\n".into();
        right.schema = "B;\nA;\n".into();
        let diff = diff_graphs(&left, &right);
        assert!(!diff.is_identical());
        assert!(render_diff("l", "r", &diff).contains("(the same lines in a different order)"));
    }

    #[test]
    fn hex_encodes_known_sha256() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
