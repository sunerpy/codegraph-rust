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

use super::{callback, is_synthesized};

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

/// The ported passes in upstream `SYNTH_PASSES` order, which is the merge
/// order: the first pass to emit a `source>target` pair keeps it.
const PASSES: &[(&str, Pass)] = &[
    ("fieldEdges", callback::field_edges),
    ("closureCollEdges", callback::closure_collection_edges),
    ("emitterEdges", callback::event_emitter_edges),
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
    let mut passes = Vec::with_capacity(PASSES.len());
    for (_, pass) in PASSES {
        passes.push(pass(&view)?);
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

/// The order a pass walks candidates in (KEEP-RUST): upstream walks SQL row
/// order, which a sync changes, and its caps and first matches follow it.
pub(super) fn node_order(a: &Node, b: &Node) -> Ordering {
    (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
        &b.file_path,
        b.start_line,
        b.start_column,
        &b.id,
    ))
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

    /// The base edges of `kind` into `target`, and the overlay's.
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
        Ok(edges)
    }

    /// The base edges of `kind` out of `source`, and the overlay's.
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
        Ok(edges)
    }

    pub(super) fn node(&self, id: &str) -> Result<Option<Node>> {
        Ok(self.store.node_by_id(id)?)
    }

    /// The nodes of `file`, by line, column and id.
    pub(super) fn nodes_in_file(&self, file: &str) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_by_file_path(file)?;
        nodes.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        Ok(nodes)
    }

    /// The nodes named `name`, in [`node_order`].
    pub(super) fn nodes_named(&self, name: &str) -> Result<Vec<Node>> {
        let mut nodes = self.store.nodes_by_name(name)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The functions and methods a name pattern can match (see
    /// [`Store::callables_named_like`]), in [`node_order`].
    pub(super) fn callables_named_like(
        &self,
        globs: &[&str],
        likes: &[&str],
        names: &[&str],
    ) -> Result<Vec<Node>> {
        let mut nodes = self.store.callables_named_like(globs, likes, names)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The functions and methods written in `languages`, in [`node_order`].
    pub(super) fn callables_in_languages(&self, languages: &[Language]) -> Result<Vec<Node>> {
        let mut nodes = self.store.callables_in_languages(languages)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// Every indexed file, by path.
    pub(super) fn files(&self) -> Result<Vec<String>> {
        let mut files: Vec<String> = self
            .store
            .all_files()?
            .into_iter()
            .map(|file| file.path)
            .collect();
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
