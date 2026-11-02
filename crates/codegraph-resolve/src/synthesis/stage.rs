//! The stage: a read-only view of the base graph, the passes in merge
//! order, the merge, and the replacement (upstream `synthesizeCallbackEdges`
//! and `SynthesisStage`).

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;
use codegraph_store::queries::ReferenceSite;

use super::{callback, dispatch, iface, is_synthesized};

/// What one run of the stage did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageStats {
    /// The merged edges the passes produced.
    pub edges: usize,
    /// The synthesized edges the replacement removed.
    pub removed: usize,
    /// The edges it inserted: the merged ones a base edge's identity did not
    /// already hold.
    pub inserted: usize,
}

/// A pass: the edges it synthesizes over the view, in its own order.
type Pass = fn(&View<'_>) -> Result<Vec<Edge>>;

/// Whether a pass can find anything in a project of these file languages
/// (upstream's `gate`): a pass it closes is provably empty, so it is
/// skipped without changing the output.
type Gate = fn(&HashSet<Language>) -> bool;

fn always(_: &HashSet<Language>) -> bool {
    true
}

/// The ported passes in upstream `SYNTH_PASSES` order, which is the merge
/// order: the first pass to emit a `source>target` pair keeps it.
const PASSES: &[(&str, Gate, Pass)] = &[
    ("fieldEdges", always, callback::field_edges),
    (
        "closureCollEdges",
        always,
        callback::closure_collection_edges,
    ),
    ("emitterEdges", always, callback::event_emitter_edges),
    ("renderEdges", always, dispatch::react_render_edges),
    ("pascalEdges", always, dispatch::pascal_form_edges),
    (
        "flutterEdges",
        |languages| languages.contains(&Language::Dart),
        dispatch::flutter_build_edges,
    ),
    (
        "cppEdges",
        |languages| languages.contains(&Language::Cpp),
        dispatch::cpp_override_edges,
    ),
    ("ifaceEdges", iface::gate, iface::interface_override_edges),
    (
        "kotlinExpectActual",
        |languages| languages.contains(&Language::Kotlin),
        dispatch::kotlin_expect_actual_edges,
    ),
    (
        "mybatisEdges",
        |languages| {
            (languages.contains(&Language::Java) || languages.contains(&Language::Kotlin))
                && languages.contains(&Language::Xml)
        },
        dispatch::mybatis_edges,
    ),
];

/// Compute every synthesized edge over `store`'s base graph and replace the
/// stored ones with them in one transaction. The marker is the caller's.
pub fn run_stage(store: &mut Store, project_root: &Path, max_file_size: u64) -> Result<StageStats> {
    let edges = synthesize(store, project_root, max_file_size)?;
    let (removed, inserted) = store.replace_synthesized_edges(&edges)?;
    Ok(StageStats {
        edges: edges.len(),
        removed,
        inserted,
    })
}

/// Every synthesized edge over `store`'s base graph: each pass's, merged in
/// registry order, sorted.
pub fn synthesize(store: &Store, project_root: &Path, max_file_size: u64) -> Result<Vec<Edge>> {
    let view = View::new(store, project_root, max_file_size);
    let languages = view.file_languages()?;
    let mut passes = Vec::with_capacity(PASSES.len());
    for (_, gate, pass) in PASSES {
        if gate(&languages) {
            passes.push(pass(&view)?);
        }
    }
    Ok(merge(passes))
}

/// Upstream's merge: each edge takes its `source>target` key, and a later
/// edge with a taken key is dropped, so the earlier pass keeps the pair. An
/// edge whose identity a base edge holds stays here and is ignored by the
/// insert (`idx_edges_identity`), still holding its key, as upstream's stage
/// trigger skips it after the merge.
fn merge(passes: Vec<Vec<Edge>>) -> Vec<Edge> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut merged: Vec<Edge> = passes
        .into_iter()
        .flatten()
        .filter(|edge| seen.insert((edge.source.clone(), edge.target.clone())))
        .collect();
    merged.sort_by(edge_order);
    merged
}

/// The order the stage writes edges in.
fn edge_order(a: &Edge, b: &Edge) -> Ordering {
    (&a.source, &a.target, a.kind.as_str(), a.line, a.col).cmp(&(
        &b.source,
        &b.target,
        b.kind.as_str(),
        b.line,
        b.col,
    ))
}

/// The order upstream's node lookups return (`ORDER BY file_path,
/// start_line, id`), which a pass's caps and first matches follow.
pub(super) fn node_order(a: &Node, b: &Node) -> Ordering {
    (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
}

/// Upstream's `methodAndFunctionNodes`: every method, then every function,
/// each in [`node_order`].
fn callable_order(a: &Node, b: &Node) -> Ordering {
    let rank = |node: &Node| node.kind != NodeKind::Method;
    rank(a).cmp(&rank(b)).then_with(|| node_order(a, b))
}

/// The order of upstream's edge lookups past the kind: by the other end,
/// then line and column (`ORDER BY kind, source, line, col` into a node,
/// `kind, target, line, col` out of one; `None` first, as SQLite's NULL).
fn edge_end_order(end: impl Fn(&Edge) -> &str) -> impl Fn(&Edge, &Edge) -> Ordering {
    move |a, b| (end(a), a.line, a.col).cmp(&(end(b), b.line, b.col))
}

/// What a pass reads: the base graph and the indexed files' text.
pub(super) struct View<'a> {
    store: &'a Store,
    project_root: PathBuf,
    max_file_size: u64,
    /// Edges the stage's prerequisite passes make visible to the later ones
    /// (W7-15's Go `contains` and `implements`); none yet.
    overlay: Vec<Edge>,
    texts: RefCell<HashMap<String, Option<Arc<str>>>>,
}

impl<'a> View<'a> {
    fn new(store: &'a Store, project_root: &Path, max_file_size: u64) -> Self {
        Self {
            store,
            project_root: project_root.to_path_buf(),
            max_file_size,
            overlay: Vec::new(),
            texts: RefCell::new(HashMap::new()),
        }
    }

    /// The base edges of `kind` into `target`, and the overlay's, by source,
    /// line and column.
    pub(super) fn edges_into(&self, target: &str, kind: EdgeKind) -> Result<Vec<Edge>> {
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_target_kind(target, Some(kind))?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(
            self.overlay
                .iter()
                .filter(|edge| edge.target == target && edge.kind == kind)
                .cloned(),
        );
        edges.sort_by(edge_end_order(|edge| &edge.source));
        Ok(edges)
    }

    /// The base edges of `kind` out of `source`, and the overlay's, by
    /// target, line and column.
    pub(super) fn edges_from(&self, source: &str, kind: EdgeKind) -> Result<Vec<Edge>> {
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_source_kind(source, Some(kind))?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(
            self.overlay
                .iter()
                .filter(|edge| edge.source == source && edge.kind == kind)
                .cloned(),
        );
        edges.sort_by(edge_end_order(|edge| &edge.target));
        Ok(edges)
    }

    /// Every base edge of `kind`, and the overlay's.
    pub(super) fn edges_of_kind(&self, kind: EdgeKind) -> Result<Vec<Edge>> {
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_kind(kind)?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(
            self.overlay
                .iter()
                .filter(|edge| edge.kind == kind)
                .cloned(),
        );
        Ok(edges)
    }

    pub(super) fn node(&self, id: &str) -> Result<Option<Node>> {
        Ok(self.store.node_by_id(id)?)
    }

    /// The nodes whose qualified name is `qualified`, in [`node_order`].
    pub(super) fn nodes_qualified(&self, qualified: &str) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_by_qualified_name(qualified)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The `language` nodes whose decorators may hold `decorator` (see
    /// [`Store::nodes_with_decorator`]), in [`node_order`].
    pub(super) fn nodes_with_decorator(
        &self,
        language: Language,
        decorator: &str,
    ) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_with_decorator(language, decorator)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The nodes of `file`, by line and id (upstream `getNodesByFile`).
    pub(super) fn nodes_in_file(&self, file: &str) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_by_file_path(file)?;
        nodes.sort_by(|a, b| (a.start_line, &a.id).cmp(&(b.start_line, &b.id)));
        Ok(nodes)
    }

    /// The nodes named `name`, in [`node_order`].
    pub(super) fn nodes_named(&self, name: &str) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_by_name(name)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The functions and methods a name pattern can match (see
    /// [`Store::callables_named_like`]): the methods, then the functions.
    pub(super) fn callables_named_like(
        &self,
        globs: &[&str],
        likes: &[&str],
        names: &[&str],
    ) -> Result<Vec<Node>> {
        let mut nodes = self.store.callables_named_like(globs, likes, names)?;
        nodes.sort_by(callable_order);
        Ok(nodes)
    }

    /// The functions and methods written in `languages`: the methods, then
    /// the functions.
    pub(super) fn callables_in_languages(&self, languages: &[Language]) -> Result<Vec<Node>> {
        let mut nodes = self.store.callables_in_languages(languages)?;
        nodes.sort_by(callable_order);
        Ok(nodes)
    }

    /// The languages of the indexed files (upstream
    /// `getDistinctFileLanguages`).
    fn file_languages(&self) -> Result<HashSet<Language>> {
        Ok(self.store.file_languages()?.into_iter().collect())
    }

    /// Every indexed file, by path (upstream `getAllFilePaths`).
    pub(super) fn files(&self) -> Result<Vec<String>> {
        let mut files = self.store.file_paths()?;
        files.sort();
        Ok(files)
    }

    /// The stored references (`unresolved_refs` and `resolved_refs`) at
    /// `site`.
    pub(super) fn refs_at(&self, site: ReferenceSite) -> Result<Vec<UnresolvedRef>> {
        Ok(self
            .store
            .stored_refs_at_sites(&[site].into_iter().collect())?)
    }

    /// `file`'s text while it is what the index holds: its size and
    /// modification time are the recorded ones, or its content hashes to the
    /// recorded hash (the MCP engine's freshness probe). A file that drifted
    /// since it was indexed is read as absent, never sliced by its old
    /// nodes' lines; the sync that reindexes it recomputes the stage.
    pub(super) fn text(&self, file: &str) -> Option<Arc<str>> {
        if let Some(cached) = self.texts.borrow().get(file) {
            return cached.clone();
        }
        let text = self.read_fresh(file);
        self.texts
            .borrow_mut()
            .insert(file.to_string(), text.clone());
        text
    }

    fn read_fresh(&self, file: &str) -> Option<Arc<str>> {
        let stored = self.store.file_by_path(file).ok().flatten()?;
        let (metadata, source) =
            read_source_file(&self.project_root.join(file), file, self.max_file_size).ok()?;
        let SourceText::Text(text) = source else {
            return None;
        };
        let stat_matches = stored.size == metadata.len() as i64
            && metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|since| since.as_millis() as i64 == stored.modified_at);
        if !stat_matches && stored.content_hash != hash_content(&text) {
            return None;
        }
        Some(Arc::from(text))
    }
}

/// The 1-based line of a byte offset into one source, its newlines indexed
/// once (upstream `makeLineAt`).
pub(super) struct LineAt {
    newlines: Vec<usize>,
    base: i64,
}

impl LineAt {
    /// Lines of `source`, which starts at line `base`.
    pub(super) fn new(source: &str, base: i64) -> Self {
        Self {
            newlines: source.match_indices('\n').map(|(at, _)| at).collect(),
            base,
        }
    }

    pub(super) fn line(&self, offset: usize) -> i64 {
        self.base + self.newlines.partition_point(|&at| at < offset) as i64
    }
}

/// Lines `start..=end` of `content`, joined back (upstream `sliceLines`);
/// `None` without both.
pub(super) fn slice_lines(content: &str, start: i64, end: i64) -> Option<String> {
    if start < 1 || end < 1 {
        return None;
    }
    let lines: Vec<&str> = content
        .split('\n')
        .skip((start - 1) as usize)
        .take((end - start + 1).max(0) as usize)
        .collect();
    Some(lines.join("\n"))
}

/// The innermost function, method or component whose lines hold `line`
/// (upstream `enclosingFn`): of those, the latest-starting, a later one in
/// `nodes`' order winning a tie.
pub(super) fn enclosing_fn(nodes: &[Node], line: i64) -> Option<&Node> {
    let mut best: Option<&Node> = None;
    for node in nodes {
        if !matches!(
            node.kind,
            NodeKind::Method | NodeKind::Function | NodeKind::Component
        ) {
            continue;
        }
        if node.start_line <= line && node.end_line.max(node.start_line) >= line {
            if best.is_none_or(|best| node.start_line >= best.start_line) {
                best = Some(node);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "cg-synthesis-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("index.db")).unwrap();
        (store, dir)
    }

    fn function(id: &str, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn synthesized(source: &str, target: &str, by: &str) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: Some(serde_json::json!({ "synthesizedBy": by })),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        }
    }

    /// Upstream's merge: the first pass to emit a `source>target` pair keeps
    /// it, whatever a later pass says of the pair; the output is sorted.
    #[test]
    fn merge_keeps_the_first_edge_of_a_pair() {
        let by = |edge: &Edge| edge.metadata.as_ref().unwrap()["synthesizedBy"].clone();
        let merged = merge(vec![
            vec![
                synthesized("c", "b", "callback"),
                synthesized("a", "b", "callback"),
            ],
            vec![
                synthesized("a", "b", "event-emitter"),
                synthesized("a", "c", "event-emitter"),
            ],
        ]);
        let rows: Vec<(String, String, serde_json::Value)> = merged
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone(), by(edge)))
            .collect();
        assert_eq!(
            rows,
            [
                ("a".to_string(), "b".to_string(), "callback".into()),
                ("a".to_string(), "c".to_string(), "event-emitter".into()),
                ("c".to_string(), "b".to_string(), "callback".into()),
            ]
        );
    }

    /// A pass reads the base graph: the stored synthesized edges of the last
    /// stage are outside the view, as a fresh index has none before it.
    #[test]
    fn the_view_reads_only_base_edges() {
        let (mut store, dir) = temp_store("view");
        store
            .upsert_nodes(&[
                function("a", "src/a.ts"),
                function("b", "src/b.ts"),
                function("c", "src/c.ts"),
            ])
            .unwrap();
        let base = Edge {
            line: Some(3),
            col: Some(1),
            metadata: None,
            provenance: None,
            ..synthesized("a", "b", "base")
        };
        store
            .insert_edges(&[
                base,
                synthesized("a", "b", "callback"),
                synthesized("c", "b", "event-emitter"),
                synthesized("a", "c", "callback"),
            ])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let ends = |edges: Vec<Edge>| -> Vec<(String, String, Option<i64>)> {
            edges
                .into_iter()
                .map(|edge| (edge.source, edge.target, edge.line))
                .collect()
        };
        let base_end = vec![("a".to_string(), "b".to_string(), Some(3))];
        assert_eq!(
            ends(view.edges_into("b", EdgeKind::Calls).unwrap()),
            base_end
        );
        assert_eq!(
            ends(view.edges_from("a", EdgeKind::Calls).unwrap()),
            base_end
        );
        assert!(view.edges_into("c", EdgeKind::Calls).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The view returns upstream's orders: a file's nodes by line and id,
    /// not column; callables method first, then by file, line and id; edges
    /// by their other end, then line, whatever order the store keeps.
    #[test]
    fn the_view_walks_upstreams_order() {
        let (mut store, dir) = temp_store("order");
        let at = |id: &str, kind: NodeKind, file: &str, line: i64, column: i64| Node {
            kind,
            name: format!("on{}", id.to_uppercase()),
            start_line: line,
            end_line: line,
            start_column: column,
            ..function(id, file)
        };
        store
            .upsert_nodes(&[
                at("b", NodeKind::Function, "src/a.ts", 3, 0),
                at("a", NodeKind::Function, "src/a.ts", 3, 9),
                at("m", NodeKind::Method, "src/z.ts", 1, 0),
                at("s1", NodeKind::Function, "src/c.ts", 1, 0),
                at("s2", NodeKind::Function, "src/c.ts", 2, 0),
            ])
            .unwrap();
        let call = |source: &str, target: &str, line: i64| Edge {
            line: Some(line),
            col: Some(0),
            metadata: None,
            provenance: None,
            ..synthesized(source, target, "base")
        };
        store
            .insert_edges(&[call("s2", "a", 1), call("s1", "b", 1), call("s1", "a", 9)])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let ids =
            |nodes: Vec<Node>| -> Vec<String> { nodes.into_iter().map(|node| node.id).collect() };
        assert_eq!(ids(view.nodes_in_file("src/a.ts").unwrap()), ["a", "b"]);
        assert_eq!(
            ids(view.callables_named_like(&["on[A-Z]*"], &[], &[]).unwrap()),
            ["m", "a", "b", "s1", "s2"]
        );
        let ends = |edges: Vec<Edge>| -> Vec<(String, String)> {
            edges
                .into_iter()
                .map(|edge| (edge.source, edge.target))
                .collect()
        };
        assert_eq!(
            ends(view.edges_into("a", EdgeKind::Calls).unwrap()),
            [
                ("s1".to_string(), "a".to_string()),
                ("s2".to_string(), "a".to_string())
            ]
        );
        assert_eq!(
            ends(view.edges_from("s1", EdgeKind::Calls).unwrap()),
            [
                ("s1".to_string(), "a".to_string()),
                ("s1".to_string(), "b".to_string())
            ]
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_at_and_slices_read_one_based_lines() {
        let source = "a\nbb\nccc\n";
        let lines = LineAt::new(source, 1);
        assert_eq!(
            [0, 1, 2, 4, 5, 8].map(|offset| lines.line(offset)),
            [1, 1, 2, 2, 3, 3]
        );
        assert_eq!(LineAt::new(source, 10).line(5), 12);
        assert_eq!(slice_lines(source, 2, 3).as_deref(), Some("bb\nccc"));
        assert_eq!(slice_lines(source, 0, 3), None);
    }
}
