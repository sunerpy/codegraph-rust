//! Real-pipeline probe harness.
//!
//! [`Project`] writes a small tree into a fresh temporary directory and runs the
//! built binary (`CARGO_BIN_EXE_codegraph`) `init` on it with
//! `CODEGRAPH_NO_DAEMON=1` and `CODEGRAPH_NO_WATCH=1`. Scanning, extraction,
//! framework detection and resolution therefore run exactly as they do for a
//! user; nothing is called in-process. [`Graph`] reads the published index back
//! through a SQLite connection opened with `immutable=1`, which takes no lock and
//! leaves no `-wal`/`-shm` sidecar: the CLI refuses an index with a foreign
//! sidecar beside it, so a probe can keep editing and syncing the project it
//! just read.
//!
//! Node selectors, the `source`/`target` arguments of the assertions, use the
//! notation the failure output prints, so a printed row can be pasted back:
//!
//! - `file:src/a.ts` is the file node of that path;
//! - `Alpha::run` is any node whose qualified name is exactly `Alpha::run`;
//! - `method Alpha::run` narrows that to one node kind;
//! - `helper @pkg/mod.py` and `import pkg.mod @main.py:1` narrow it to the
//!   nodes of one file, and of one start line.
//!
//! `tests/sync_convergence/main.rs` includes this file through `#[path]`, so
//! each of the two test binaries uses only part of it.
#![allow(dead_code, unused_imports, unused_macros)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use assert_fs::TempDir;
use codegraph_core::IndexPaths;
use codegraph_core::types::{EdgeKind, NodeKind};
use rusqlite::{Connection, OpenFlags, Row};
use serde_json::Value;

/// Modification time of every file written before a project's first index.
///
/// Each later write moves its file to a strictly later [`edit_mtime`], so a
/// sync never depends on the filesystem's timestamp granularity to notice a
/// same-size edit, and no test sleeps.
pub fn base_mtime() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000)
}

/// Modification time of the `generation`-th edit of a project (1-based).
pub fn edit_mtime(generation: u32) -> SystemTime {
    base_mtime() + Duration::from_secs(60 * u64::from(generation))
}

pub fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

/// Runs the built `codegraph` with the daemon and the watcher disabled, in
/// `cwd`, and returns its output. A non-zero exit panics with both streams.
#[track_caller]
pub fn codegraph<I, S>(cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<OsString>>();
    let output = Command::new(bin())
        .args(&args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .unwrap_or_else(|err| panic!("spawn {}: {err}", bin().display()));
    assert!(
        output.status.success(),
        "`codegraph {}` exited with {}\n--- stdout\n{}--- stderr\n{}",
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

/// The index paths the CLI and the in-process sync resolve for `root`.
#[track_caller]
pub fn index_paths(root: &Path) -> IndexPaths {
    IndexPaths::resolve(root, std::env::var("CODEGRAPH_DIR").ok().as_deref())
        .unwrap_or_else(|err| panic!("resolve the index paths of {}: {err}", root.display()))
}

pub fn db_path(root: &Path) -> PathBuf {
    index_paths(root).current_db()
}

/// A fixture path stays inside its project: relative, `/`-separated, with no
/// empty, `.` or `..` component. Returns it in the platform's own form.
#[track_caller]
pub fn checked_relative(relative: &str) -> PathBuf {
    let plain = !relative.is_empty()
        && !relative.starts_with('/')
        && !relative.contains('\\')
        && !relative.contains(':')
        && relative
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    assert!(
        plain,
        "fixture path `{relative}` must be a plain relative `/` path inside the project"
    );
    relative.split('/').collect()
}

/// Writes `contents` to `root/relative`, creating parent directories, and
/// stamps the file with `mtime`.
#[track_caller]
pub fn write_file(root: &Path, relative: &str, contents: &str, mtime: SystemTime) {
    let path = root.join(checked_relative(relative));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|err| panic!("create {}: {err}", parent.display()));
    }
    fs::write(&path, contents).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    set_mtime(&path, mtime);
}

#[track_caller]
pub fn set_mtime(path: &Path, mtime: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(mtime))
        .unwrap_or_else(|err| panic!("set the mtime of {}: {err}", path.display()));
}

/// A project tree in its own temporary directory, not yet indexed.
pub struct Project {
    _dir: TempDir,
    root: PathBuf,
}

impl Default for Project {
    fn default() -> Self {
        Self::new()
    }
}

impl Project {
    pub fn new() -> Self {
        Self::named("project")
    }

    /// A project whose root directory is called `label`, which failure output
    /// and stray-process listings show.
    pub fn named(label: &str) -> Self {
        let dir = TempDir::new().expect("create a temporary project directory");
        let root = dir.path().join(checked_relative(label));
        fs::create_dir_all(&root).unwrap_or_else(|err| panic!("create {}: {err}", root.display()));
        Self { _dir: dir, root }
    }

    #[track_caller]
    pub fn file(self, relative: &str, contents: &str) -> Self {
        write_file(&self.root, relative, contents, base_mtime());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs `codegraph init` on the tree and reads the published graph.
    #[track_caller]
    pub fn index(self) -> Indexed {
        codegraph(&self.root, [OsStr::new("init"), self.root.as_os_str()]);
        let graph = Graph::load(&db_path(&self.root));
        Indexed {
            project: self,
            graph,
            generation: 0,
        }
    }
}

/// An indexed project and the graph last read from it.
pub struct Indexed {
    project: Project,
    graph: Graph,
    generation: u32,
}

impl Indexed {
    pub fn root(&self) -> &Path {
        self.project.root()
    }

    pub fn db(&self) -> PathBuf {
        db_path(self.root())
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Adds or rewrites one file with a modification time no earlier write of
    /// this project used. The index is not touched until [`Self::sync`].
    #[track_caller]
    pub fn write(&mut self, relative: &str, contents: &str) -> &mut Self {
        self.generation += 1;
        write_file(
            self.project.root(),
            relative,
            contents,
            edit_mtime(self.generation),
        );
        self
    }

    #[track_caller]
    pub fn remove(&mut self, relative: &str) -> &mut Self {
        let path = self.root().join(checked_relative(relative));
        fs::remove_file(&path).unwrap_or_else(|err| panic!("remove {}: {err}", path.display()));
        self
    }

    /// Renames one file. The moved file keeps its modification time, as a
    /// rename on disk does.
    #[track_caller]
    pub fn rename(&mut self, from: &str, to: &str) -> &mut Self {
        let from = self.root().join(checked_relative(from));
        let to = self.root().join(checked_relative(to));
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|err| panic!("create {}: {err}", parent.display()));
        }
        fs::rename(&from, &to)
            .unwrap_or_else(|err| panic!("rename {} to {}: {err}", from.display(), to.display()));
        self
    }

    /// Runs `codegraph sync` and re-reads the graph.
    #[track_caller]
    pub fn sync(&mut self) -> &Graph {
        codegraph(self.root(), [OsStr::new("sync"), self.root().as_os_str()]);
        self.reload()
    }

    /// Re-reads the graph after something other than this harness updated the
    /// index.
    #[track_caller]
    pub fn reload(&mut self) -> &Graph {
        self.graph = Graph::load(&self.db());
        &self.graph
    }
}

impl AsRef<Graph> for Indexed {
    fn as_ref(&self) -> &Graph {
        &self.graph
    }
}

impl AsRef<Graph> for Graph {
    fn as_ref(&self) -> &Graph {
        self
    }
}

/// What the assertion macros accept: an [`Indexed`] project or a [`Graph`].
pub fn graph_of<G: AsRef<Graph> + ?Sized>(graph: &G) -> &Graph {
    graph.as_ref()
}

#[derive(Clone, Debug)]
pub struct NodeRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: i64,
    pub end_line: i64,
}

#[derive(Clone, Debug)]
pub struct EdgeRow {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub metadata: Value,
    pub line: Option<i64>,
    pub col: Option<i64>,
    pub provenance: Option<String>,
}

impl EdgeRow {
    pub fn resolved_by(&self) -> Option<&str> {
        self.metadata.get("resolvedBy").and_then(Value::as_str)
    }

    pub fn confidence(&self) -> Option<f64> {
        self.metadata.get("confidence").and_then(Value::as_f64)
    }
}

#[derive(Clone, Debug)]
pub struct RefRow {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    pub line: i64,
    pub col: i64,
    pub file_path: String,
    pub language: String,
    pub candidates: Value,
    pub subkind: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FileRow {
    pub path: String,
    pub language: String,
    pub node_count: i64,
    pub errors: Value,
}

/// Every row of one published index, read once.
#[derive(Clone, Debug)]
pub struct Graph {
    pub nodes: Vec<NodeRow>,
    pub edges: Vec<EdgeRow>,
    pub unresolved: Vec<RefRow>,
    pub files: Vec<FileRow>,
    by_id: BTreeMap<String, usize>,
}

impl Graph {
    /// Reads every node, edge, unresolved reference and file of `db` through
    /// an immutable connection.
    #[track_caller]
    pub fn load(db: &Path) -> Self {
        let conn = open_immutable(db);
        let nodes = rows(
            &conn,
            "SELECT id, kind, name, qualified_name, file_path, language, start_line, end_line \
             FROM nodes ORDER BY id",
            |row| {
                Ok(NodeRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    name: row.get(2)?,
                    qualified_name: row.get(3)?,
                    file_path: row.get(4)?,
                    language: row.get(5)?,
                    start_line: row.get(6)?,
                    end_line: row.get(7)?,
                })
            },
        );
        let edges = rows(
            &conn,
            "SELECT source, target, kind, metadata, line, col, provenance FROM edges \
             ORDER BY source, kind, target, line, col",
            |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    target: row.get(1)?,
                    kind: row.get(2)?,
                    metadata: json_column(row, 3)?,
                    line: row.get(4)?,
                    col: row.get(5)?,
                    provenance: row.get(6)?,
                })
            },
        );
        let unresolved = rows(
            &conn,
            "SELECT from_node_id, reference_name, reference_kind, line, col, file_path, \
             language, candidates, reference_subkind FROM unresolved_refs \
             ORDER BY file_path, line, col, reference_kind, reference_name",
            |row| {
                Ok(RefRow {
                    from_node_id: row.get(0)?,
                    reference_name: row.get(1)?,
                    reference_kind: row.get(2)?,
                    line: row.get(3)?,
                    col: row.get(4)?,
                    file_path: row.get(5)?,
                    language: row.get(6)?,
                    candidates: json_column(row, 7)?,
                    subkind: row.get(8)?,
                })
            },
        );
        let files = rows(
            &conn,
            "SELECT path, language, node_count, errors FROM files ORDER BY path",
            |row| {
                Ok(FileRow {
                    path: row.get(0)?,
                    language: row.get(1)?,
                    node_count: row.get(2)?,
                    errors: json_column(row, 3)?,
                })
            },
        );
        let by_id = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.clone(), index))
            .collect();
        Self {
            nodes,
            edges,
            unresolved,
            files,
            by_id,
        }
    }

    pub fn node(&self, id: &str) -> Option<&NodeRow> {
        self.by_id.get(id).map(|&index| &self.nodes[index])
    }

    /// Every node `selector` matches.
    #[track_caller]
    pub fn nodes_matching(&self, selector: &str) -> Vec<&NodeRow> {
        let selector = Selector::parse(selector);
        self.nodes
            .iter()
            .filter(|node| selector.matches(node))
            .collect()
    }

    /// Every node of `kind` (a [`NodeKind`] name) whose bare name is `name`.
    #[track_caller]
    pub fn nodes_named(&self, kind: &str, name: &str) -> Vec<&NodeRow> {
        let kind = node_kind(kind);
        self.nodes
            .iter()
            .filter(|node| node.kind == kind && node.name == name)
            .collect()
    }

    /// Every edge of `kind` (an [`EdgeKind`] name) from a node matching
    /// `source` to a node matching `target`.
    #[track_caller]
    pub fn edges_matching(&self, source: &str, kind: &str, target: &str) -> Vec<&EdgeRow> {
        let kind = edge_kind(kind);
        let source = Selector::parse(source);
        let target = Selector::parse(target);
        self.edges
            .iter()
            .filter(|edge| {
                edge.kind == kind
                    && self
                        .node(&edge.source)
                        .is_some_and(|node| source.matches(node))
                    && self
                        .node(&edge.target)
                        .is_some_and(|node| target.matches(node))
            })
            .collect()
    }

    /// Every unresolved reference of `kind` named `name` whose source node
    /// matches `from`.
    #[track_caller]
    pub fn unresolved_matching(&self, from: &str, kind: &str, name: &str) -> Vec<&RefRow> {
        let from = Selector::parse(from);
        self.unresolved
            .iter()
            .filter(|reference| {
                reference.reference_kind == kind
                    && reference.reference_name == name
                    && self
                        .node(&reference.from_node_id)
                        .is_some_and(|node| from.matches(node))
            })
            .collect()
    }

    /// `node` in selector notation.
    pub fn describe(&self, id: &str) -> String {
        match self.node(id) {
            Some(node) if node.kind == NodeKind::File.as_str() => node.id.clone(),
            Some(node) => format!(
                "{} {} @{}:{}",
                node.kind, node.qualified_name, node.file_path, node.start_line
            ),
            None => format!("<no node {id}>"),
        }
    }

    pub fn describe_edge(&self, edge: &EdgeRow) -> String {
        let mut line = format!(
            "{} -[{}]-> {}",
            self.describe(&edge.source),
            edge.kind,
            self.describe(&edge.target)
        );
        if let (Some(at), Some(col)) = (edge.line, edge.col) {
            let _ = write!(line, "  at {at}:{col}");
        }
        if let Some(by) = edge.resolved_by() {
            let _ = write!(line, "  {by}");
        }
        if let Some(confidence) = edge.confidence() {
            let _ = write!(line, " {confidence}");
        }
        if let Some(provenance) = &edge.provenance {
            let _ = write!(line, "  provenance={provenance}");
        }
        line
    }

    pub fn describe_ref(&self, reference: &RefRow) -> String {
        let mut line = format!(
            "{} -[{}]-> ? {}  at {}:{}",
            self.describe(&reference.from_node_id),
            reference.reference_kind,
            reference.reference_name,
            reference.line,
            reference.col
        );
        if !reference.candidates.is_null() {
            let _ = write!(line, "  candidates={}", reference.candidates);
        }
        if let Some(subkind) = &reference.subkind {
            let _ = write!(line, "  subkind={subkind}");
        }
        line
    }

    /// Every edge except `contains`, one per line in selector notation, sorted.
    pub fn edge_list(&self) -> String {
        let mut lines = self
            .edges
            .iter()
            .filter(|edge| edge.kind != EdgeKind::Contains.as_str())
            .map(|edge| self.describe_edge(edge))
            .collect::<Vec<_>>();
        lines.sort();
        listing("non-contains edges", &lines)
    }

    /// Every unresolved reference, one per line, sorted.
    pub fn unresolved_list(&self) -> String {
        let mut lines = self
            .unresolved
            .iter()
            .map(|reference| self.describe_ref(reference))
            .collect::<Vec<_>>();
        lines.sort();
        listing("unresolved references", &lines)
    }

    /// The project's whole graph: non-contains edges, unresolved references and
    /// files with their languages.
    pub fn report(&self) -> String {
        let files = self
            .files
            .iter()
            .map(|file| {
                if file.errors.is_null() {
                    format!("{} ({})", file.path, file.language)
                } else {
                    format!("{} ({}) errors={}", file.path, file.language, file.errors)
                }
            })
            .collect::<Vec<_>>();
        format!(
            "{}{}{}",
            self.edge_list(),
            self.unresolved_list(),
            listing("files", &files)
        )
    }

    /// Panics unless an edge of `kind` joins a node matching `source` to a node
    /// matching `target` (with `metadata.resolvedBy == resolved_by`, if given).
    #[track_caller]
    pub fn assert_edge(&self, source: &str, kind: &str, target: &str, resolved_by: Option<&str>) {
        let found = self.edges_matching(source, kind, target);
        let satisfied = match resolved_by {
            None => !found.is_empty(),
            Some(by) => found.iter().any(|edge| edge.resolved_by() == Some(by)),
        };
        if !satisfied {
            let wanted = match resolved_by {
                None => format!("{source} -[{kind}]-> {target}"),
                Some(by) => format!("{source} -[{kind}]-> {target} resolved by `{by}`"),
            };
            panic!(
                "expected an edge {wanted}\n{}{}",
                self.selector_report(&[source, target], &found),
                self.report()
            );
        }
    }

    /// Panics if any edge of `kind` joins a node matching `source` to a node
    /// matching `target`. Both selectors must match a node: a negative
    /// assertion over a node that does not exist proves nothing.
    #[track_caller]
    pub fn assert_no_edge(&self, source: &str, kind: &str, target: &str) {
        for selector in [source, target] {
            if self.nodes_matching(selector).is_empty() {
                panic!(
                    "`{selector}` matches no node, so `assert_no_edge` would hold vacuously\n{}",
                    self.report()
                );
            }
        }
        let found = self.edges_matching(source, kind, target);
        if !found.is_empty() {
            panic!(
                "expected no edge {source} -[{kind}]-> {target}\n{}{}",
                self.selector_report(&[source, target], &found),
                self.report()
            );
        }
    }

    /// Panics unless an unresolved reference of `kind` named `name` leaves a
    /// node matching `from`.
    #[track_caller]
    pub fn assert_unresolved(&self, from: &str, kind: &str, name: &str) {
        if self.unresolved_matching(from, kind, name).is_empty() {
            panic!(
                "expected an unresolved reference {from} -[{kind}]-> ? {name}\n{}{}",
                self.selector_report(&[from], &[]),
                self.report()
            );
        }
    }

    fn selector_report(&self, selectors: &[&str], found: &[&EdgeRow]) -> String {
        let mut report = String::new();
        for selector in selectors {
            let matched = self
                .nodes_matching(selector)
                .into_iter()
                .map(|node| self.describe(&node.id))
                .collect::<Vec<_>>();
            if matched.is_empty() {
                // Point at the nodes the selector probably meant.
                let wanted = Selector::parse(selector);
                let bare = wanted
                    .qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(wanted.qualified_name);
                let near = self
                    .nodes
                    .iter()
                    .filter(|node| node.name == bare || node.qualified_name.ends_with(bare))
                    .map(|node| self.describe(&node.id))
                    .collect::<Vec<_>>();
                report.push_str(&listing(
                    &format!("`{selector}` matches no node; nodes named like it"),
                    &near,
                ));
            } else {
                report.push_str(&listing(&format!("`{selector}` matches"), &matched));
            }
        }
        if !found.is_empty() {
            let lines = found
                .iter()
                .map(|edge| self.describe_edge(edge))
                .collect::<Vec<_>>();
            report.push_str(&listing("matching edges", &lines));
        }
        report
    }
}

fn listing(title: &str, lines: &[String]) -> String {
    let mut text = format!("{title} ({}):\n", lines.len());
    for line in lines {
        let _ = writeln!(text, "    {line}");
    }
    text
}

/// A parsed node selector; see the module documentation for the notation.
struct Selector<'a> {
    file_node: Option<&'a str>,
    kind: Option<&'a str>,
    qualified_name: &'a str,
    file: Option<&'a str>,
    line: Option<i64>,
}

impl<'a> Selector<'a> {
    #[track_caller]
    fn parse(text: &'a str) -> Self {
        if text.starts_with("file:") {
            return Self {
                file_node: Some(text),
                kind: None,
                qualified_name: text,
                file: None,
                line: None,
            };
        }
        let (body, location) = match text.rsplit_once(" @") {
            Some((body, location)) => (body, Some(location)),
            None => (text, None),
        };
        let (file, line) = match location {
            None => (None, None),
            Some(location) => match location.rsplit_once(':') {
                Some((file, line))
                    if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    (Some(file), line.parse().ok())
                }
                _ => (Some(location), None),
            },
        };
        let (kind, qualified_name) = match body.split_once(' ') {
            Some((kind, rest)) if NodeKind::ALL.iter().any(|known| known.as_str() == kind) => {
                (Some(kind), rest)
            }
            _ => (None, body),
        };
        assert!(
            !qualified_name.is_empty() && file.is_none_or(|file| !file.is_empty()),
            "malformed node selector `{text}`"
        );
        Self {
            file_node: None,
            kind,
            qualified_name,
            file,
            line,
        }
    }

    fn matches(&self, node: &NodeRow) -> bool {
        if let Some(id) = self.file_node {
            return node.id == id;
        }
        node.qualified_name == self.qualified_name
            && self.kind.is_none_or(|kind| node.kind == kind)
            && self.file.is_none_or(|file| node.file_path == file)
            && self.line.is_none_or(|line| node.start_line == line)
    }
}

#[track_caller]
fn edge_kind(kind: &str) -> &str {
    assert!(
        EdgeKind::ALL.iter().any(|known| known.as_str() == kind),
        "`{kind}` is not an edge kind; known kinds: {:?}",
        EdgeKind::ALL.map(EdgeKind::as_str)
    );
    kind
}

#[track_caller]
fn node_kind(kind: &str) -> &str {
    assert!(
        NodeKind::ALL.iter().any(|known| known.as_str() == kind),
        "`{kind}` is not a node kind; known kinds: {:?}",
        NodeKind::ALL.map(NodeKind::as_str)
    );
    kind
}

/// Opens `db` read-only with `immutable=1`: no lock, no `-wal`/`-shm`.
///
/// An immutable connection does not read a write-ahead log, so a non-empty one
/// beside the database would make it miss committed rows. The CLI checkpoints
/// and removes its log when it exits, so one here is a harness error.
#[track_caller]
fn open_immutable(db: &Path) -> Connection {
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    if let Ok(meta) = fs::metadata(&wal) {
        assert!(
            meta.len() == 0,
            "{} has a non-empty write-ahead log; an immutable read would miss rows",
            db.display()
        );
    }
    let uri = format!("{}?immutable=1", sqlite_file_uri(db));
    Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|err| panic!("open {uri}: {err}"))
}

/// `path` as an absolute SQLite `file:` URI. A Windows verbatim prefix
/// (`\\?\`) is dropped, and every byte outside the unreserved set is
/// percent-encoded.
fn sqlite_file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

#[track_caller]
fn rows<T>(
    conn: &Connection,
    sql: &str,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    let mut statement = conn
        .prepare(sql)
        .unwrap_or_else(|err| panic!("prepare `{sql}`: {err}"));
    statement
        .query_map([], map)
        .and_then(|mapped| mapped.collect::<rusqlite::Result<Vec<T>>>())
        .unwrap_or_else(|err| panic!("query `{sql}`: {err}"))
}

fn json_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let text: Option<String> = row.get(index)?;
    Ok(text
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null))
}

/// `assert_edge!(graph, "source" => kind => "target")` panics unless an edge
/// of `kind` joins a node matching `source` to a node matching `target`;
/// `, resolved_by = "import"` also requires that `metadata.resolvedBy`.
/// The failure lists the project's whole non-contains edge list.
macro_rules! assert_edge {
    ($graph:expr, $source:expr => $kind:ident => $target:expr $(,)?) => {
        $crate::support::graph_of(&$graph).assert_edge($source, stringify!($kind), $target, None)
    };
    ($graph:expr, $source:expr => $kind:ident => $target:expr, resolved_by = $by:expr $(,)?) => {
        $crate::support::graph_of(&$graph).assert_edge(
            $source,
            stringify!($kind),
            $target,
            Some($by),
        )
    };
}
pub(crate) use assert_edge;

/// `assert_no_edge!(graph, "source" => kind => "target")` panics if such an
/// edge exists, or if either selector matches no node.
macro_rules! assert_no_edge {
    ($graph:expr, $source:expr => $kind:ident => $target:expr $(,)?) => {
        $crate::support::graph_of(&$graph).assert_no_edge($source, stringify!($kind), $target)
    };
}
pub(crate) use assert_no_edge;

/// `assert_unresolved!(graph, "from" => kind => "name")` panics unless an
/// unresolved reference of `kind` named `name` leaves a node matching `from`.
macro_rules! assert_unresolved {
    ($graph:expr, $from:expr => $kind:ident => $name:expr $(,)?) => {
        $crate::support::graph_of(&$graph).assert_unresolved($from, stringify!($kind), $name)
    };
}
pub(crate) use assert_unresolved;
