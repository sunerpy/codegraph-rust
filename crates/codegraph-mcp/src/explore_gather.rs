//! The explore subgraph walk (W7-25a).
//!
//! Upstream `findRelevantContext` gathers explore's subgraph in two halves: a
//! multi-channel search for the entry points, then a walk from them
//! (`src/context/index.ts`, `e8ddc4e2` 1111-1313, the same at `b635dd46`):
//! each class-like entry's type hierarchy, a breadth-first walk from every
//! entry, a trim to the node budget, a cap per file and on non-production
//! files, and the edges among what was kept. This module is the walk. The
//! entry points stay the Rust search's ([`codegraph_graph::query::search_nodes`]
//! with upstream's kind filter, [`HIGH_VALUE_KINDS`]); upstream's search
//! channels are not ported (KNOWN_DIFFS).
//!
//! Every edge read here is put in upstream's order (`getOutgoingEdges` reads
//! `ORDER BY kind, target, line, col`, `getIncomingEdges` `ORDER BY kind,
//! source, line, col`), so a synced index and a fresh one, which store their
//! rows in different orders, gather the same subgraph.
//!
//! The walk reads each node's edges and each node's row at most once
//! ([`WalkReads`], W7-25 cost, OD-B75): the entry points' walks share what
//! they read, a row is read only while a walk still has room for it, and the
//! glue after the walk answers from the same reads. The engine holds its
//! shared lease for the whole request, which keeps every writer out, so a
//! second read would return the same rows.

use std::collections::{HashMap, HashSet, VecDeque};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_graph::query::scoring::is_test_file as is_non_production_file;
use codegraph_store::Store;

/// Upstream `HIGH_VALUE_NODE_KINDS` (`context/index.ts` 168-171): the kinds
/// explore's search returns and its walk steps onto. Files, imports,
/// exports, members and parameters say something exists, not how it works.
pub(crate) const HIGH_VALUE_KINDS: [NodeKind; 15] = [
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::TypeAlias,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Trait,
    NodeKind::Component,
    NodeKind::Route,
    NodeKind::Variable,
    NodeKind::Constant,
    NodeKind::Enum,
    NodeKind::Module,
    NodeKind::Namespace,
];

/// Explore's `maxNodes` (`tools.ts` b635dd46 4256-4261).
const MAX_NODES: usize = 200;

/// Explore's `traversalDepth`.
const TRAVERSAL_DEPTH: usize = 3;

/// The kinds whose type hierarchy is expanded before the walk
/// (`typeHierarchyKinds`, `context/index.ts` 1115).
const HIERARCHY_KINDS: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Trait,
    NodeKind::Protocol,
];

/// The edges recovered among the kept nodes (`recoveryKinds`,
/// `context/index.ts` 1297).
const RECOVERY_KINDS: [EdgeKind; 6] = [
    EdgeKind::Calls,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::References,
    EdgeKind::Overrides,
    EdgeKind::Navigates,
];

/// What upstream's `getCallers`/`getCallees` follow (`graph/traversal.ts`
/// e8ddc4e2 279-381).
const CALLER_KINDS: [EdgeKind; 5] = [
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Imports,
    EdgeKind::Instantiates,
    EdgeKind::Navigates,
];

/// An edge's identity, as the edges table's unique index reads it.
type EdgeIdentity = (String, String, EdgeKind, Option<i64>, Option<i64>);

/// The extends/implements edges a type hierarchy follows.
const HIERARCHY_EDGES: [EdgeKind; 2] = [EdgeKind::Extends, EdgeKind::Implements];

/// An insertion-ordered set of nodes and their edges, the edges kept once per
/// `(source, target, kind)` as `findRelevantContext` merges them.
#[derive(Default)]
pub(crate) struct Gathered {
    pub(crate) nodes: Vec<Node>,
    index: HashMap<String, usize>,
    pub(crate) edges: Vec<Edge>,
    edge_keys: HashSet<(String, String, EdgeKind)>,
}

impl Gathered {
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Adds `node` unless it is already here; whether it was added.
    pub(crate) fn insert(&mut self, node: Node) -> bool {
        if self.index.contains_key(&node.id) {
            return false;
        }
        self.index.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
        true
    }

    pub(crate) fn add_edge(&mut self, edge: Edge) {
        let key = (edge.source.clone(), edge.target.clone(), edge.kind);
        if self.edge_keys.insert(key) {
            self.edges.push(edge);
        }
    }

    fn has_edge(&self, edge: &Edge) -> bool {
        self.edge_keys
            .contains(&(edge.source.clone(), edge.target.clone(), edge.kind))
    }

    /// Keeps the nodes in `keep` in their order, and the edges between them.
    fn retain(&mut self, keep: &HashSet<String>) {
        self.nodes.retain(|node| keep.contains(&node.id));
        self.index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(at, node)| (node.id.clone(), at))
            .collect();
        self.retain_edges();
    }

    /// Drops the edges whose source or target is no longer a node here.
    fn retain_edges(&mut self) {
        let index = &self.index;
        self.edges
            .retain(|edge| index.contains_key(&edge.source) && index.contains_key(&edge.target));
        self.edge_keys = self
            .edges
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone(), edge.kind))
            .collect();
    }
}

/// The walk's result: the gathered subgraph, the entry points that survived
/// the non-production cap, in their rank order, and what the walk read.
pub(crate) struct Walk {
    pub(crate) gathered: Gathered,
    pub(crate) roots: Vec<String>,
    pub(crate) reads: WalkReads,
}

/// The rows one batch of a visit reads at first; each further batch of the
/// same visit reads twice as many.
const FIRST_ROW_BATCH: usize = 32;

/// What one explore's walk read from the store, so that nothing is read twice
/// (W7-25 cost, OD-B75): every visited node's edges of every kind, in
/// upstream's order, and every node row looked up (`None`: no such node).
/// The entry points' walks share it, and the glue answers from it.
#[derive(Default)]
pub(crate) struct WalkReads {
    edges: HashMap<String, (Vec<Edge>, Vec<Edge>)>,
    rows: HashMap<String, Option<Node>>,
}

impl WalkReads {
    /// `id`'s outgoing and incoming edges of every kind, in the orders of
    /// [`outgoing_in_order`] and [`incoming_in_order`], read once.
    fn edges(&mut self, store: &Store, id: &str) -> anyhow::Result<(Vec<Edge>, Vec<Edge>)> {
        if let Some((outgoing, incoming)) = self.edges.get(id) {
            return Ok((outgoing.clone(), incoming.clone()));
        }
        #[cfg(test)]
        test_reads::note_edges(id);
        let outgoing = outgoing_in_order(store, id, &[])?;
        let incoming = incoming_in_order(store, id, &[])?;
        self.edges
            .insert(id.to_string(), (outgoing.clone(), incoming.clone()));
        Ok((outgoing, incoming))
    }

    /// Reads, in one query, the rows of those of `ids` not read yet.
    fn read_rows(&mut self, store: &Store, ids: &[String]) -> anyhow::Result<()> {
        let mut missing: Vec<String> = Vec::new();
        let mut picked: HashSet<&str> = HashSet::new();
        for id in ids {
            if !self.rows.contains_key(id) && picked.insert(id) {
                missing.push(id.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        for id in &missing {
            test_reads::note_row(id);
        }
        let mut found = store.nodes_by_ids(&missing)?;
        for id in missing {
            let row = found.remove(&id);
            self.rows.insert(id, row);
        }
        Ok(())
    }

    fn has_row(&self, id: &str) -> bool {
        self.rows.contains_key(id)
    }

    fn row(&self, id: &str) -> Option<&Node> {
        self.rows.get(id).and_then(Option::as_ref)
    }

    /// [`callers`] of `id`, answered from its edges when the walk visited it
    /// (every entry point is visited first by its own walk). Keeping only
    /// [`CALLER_KINDS`] leaves the kept edges in their order; edges that tie
    /// on the order's whole key share their caller, so the deduplicated list
    /// is the one [`callers`] reads.
    pub(crate) fn callers(&mut self, store: &Store, id: &str) -> anyhow::Result<Vec<Node>> {
        let Some((_, incoming)) = self.edges.get(id) else {
            return callers(store, id);
        };
        let ends = incoming
            .iter()
            .filter(|edge| CALLER_KINDS.contains(&edge.kind))
            .map(|edge| edge.source.clone())
            .collect::<Vec<_>>();
        self.rows_in_order(store, distinct_ends(id, ends))
    }

    /// [`callees`] of `id`, the mirror of [`Self::callers`].
    pub(crate) fn callees(&mut self, store: &Store, id: &str) -> anyhow::Result<Vec<Node>> {
        let Some((outgoing, _)) = self.edges.get(id) else {
            return callees(store, id);
        };
        let ends = outgoing
            .iter()
            .filter(|edge| CALLER_KINDS.contains(&edge.kind))
            .map(|edge| edge.target.clone())
            .collect::<Vec<_>>();
        self.rows_in_order(store, distinct_ends(id, ends))
    }

    /// The nodes of `ids` that exist, in that order.
    fn rows_in_order(&mut self, store: &Store, ids: Vec<String>) -> anyhow::Result<Vec<Node>> {
        self.read_rows(store, &ids)?;
        Ok(ids.iter().filter_map(|id| self.row(id).cloned()).collect())
    }
}

/// Upstream's walk from the entry points `roots` (in rank order), the steps
/// of `context/index.ts` 1111-1306. `test_query` is upstream's `isTestQuery`:
/// the query mentions `test` or `spec` anywhere (`context/index.ts` 713).
pub(crate) fn walk(store: &Store, roots: &[Node], test_query: bool) -> anyhow::Result<Walk> {
    let mut gathered = Gathered::default();
    let mut root_ids: Vec<String> = Vec::new();
    for root in roots {
        if gathered.insert(root.clone()) {
            root_ids.push(root.id.clone());
        }
    }
    expand_type_hierarchies(store, &mut gathered, &root_ids)?;
    let per_root = MAX_NODES.div_ceil(root_ids.len().max(1));
    let mut reads = WalkReads::default();
    for root in roots {
        let (nodes, edges) = breadth_first(store, root, TRAVERSAL_DEPTH, per_root, &mut reads)?;
        for node in nodes {
            gathered.insert(node);
        }
        for edge in edges {
            gathered.add_edge(edge);
        }
    }
    trim_to_budget(&mut gathered, &root_ids);
    cap_per_file(&mut gathered, &root_ids);
    if !test_query {
        cap_non_production(&mut gathered, &mut root_ids);
    }
    gathered.retain_edges();
    let ids: Vec<String> = gathered.nodes.iter().map(|node| node.id.clone()).collect();
    for edge in store.edges_among(&ids, &RECOVERY_KINDS)? {
        gathered.add_edge(edge);
    }
    Ok(Walk {
        gathered,
        roots: root_ids,
        reads,
    })
}

/// Each class-like entry's type hierarchy, then the hierarchy of every type
/// the first pass found, up to `⌈maxNodes / 4⌉` added nodes
/// (`context/index.ts` 1111-1165). Upstream checks the budget between
/// hierarchies in the first pass and per node in the second, and so does this.
fn expand_type_hierarchies(
    store: &Store,
    gathered: &mut Gathered,
    root_ids: &[String],
) -> anyhow::Result<()> {
    let budget = MAX_NODES.div_ceil(4);
    let mut added = 0usize;
    for root in root_ids {
        if added >= budget {
            break;
        }
        let Some(kind) = gathered
            .nodes
            .iter()
            .find(|n| &n.id == root)
            .map(|n| n.kind)
        else {
            continue;
        };
        if !HIERARCHY_KINDS.contains(&kind) {
            continue;
        }
        let (nodes, edges) = type_hierarchy(store, root)?;
        for node in nodes {
            if gathered.insert(node) {
                added += 1;
            }
        }
        for edge in edges {
            gathered.add_edge(edge);
        }
    }
    if added == 0 {
        return Ok(());
    }
    let candidates: Vec<String> = gathered
        .nodes
        .iter()
        .filter(|n| HIERARCHY_KINDS.contains(&n.kind) && !root_ids.contains(&n.id))
        .map(|n| n.id.clone())
        .collect();
    for candidate in candidates {
        if added >= budget {
            break;
        }
        let (nodes, edges) = type_hierarchy(store, &candidate)?;
        for node in nodes {
            if added < budget && gathered.insert(node) {
                added += 1;
            }
        }
        for edge in edges {
            if gathered.contains(&edge.source)
                && gathered.contains(&edge.target)
                && !gathered.has_edge(&edge)
            {
                gathered.add_edge(edge);
            }
        }
    }
    Ok(())
}

/// Upstream `getTypeHierarchy` (`graph/traversal.ts` 430-500): the node and
/// its ancestors depth first. Upstream then asks for the descendants with the
/// SAME visited set, which already holds the node, so none are added; the
/// Rust `GraphTraverser::get_type_hierarchy` does the same, and so does this.
fn type_hierarchy(store: &Store, id: &str) -> anyhow::Result<(Vec<Node>, Vec<Edge>)> {
    let Some(focal) = store.node_by_id(id)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut nodes = Gathered::default();
    let mut edges = Vec::new();
    nodes.insert(focal);
    let mut visited = HashSet::new();
    type_relatives(store, id, true, &mut nodes, &mut edges, &mut visited)?;
    type_relatives(store, id, false, &mut nodes, &mut edges, &mut visited)?;
    Ok((nodes.nodes, edges))
}

fn type_relatives(
    store: &Store,
    id: &str,
    ancestors: bool,
    nodes: &mut Gathered,
    edges: &mut Vec<Edge>,
    visited: &mut HashSet<String>,
) -> anyhow::Result<()> {
    if !visited.insert(id.to_string()) {
        return Ok(());
    }
    let related = if ancestors {
        outgoing_in_order(store, id, &HIERARCHY_EDGES)?
    } else {
        incoming_in_order(store, id, &HIERARCHY_EDGES)?
    };
    if related.is_empty() {
        return Ok(());
    }
    let ends: Vec<String> = related
        .iter()
        .map(|e| {
            if ancestors {
                e.target.clone()
            } else {
                e.source.clone()
            }
        })
        .collect();
    let found = store.nodes_by_ids(&ends)?;
    for edge in related {
        let end = if ancestors {
            &edge.target
        } else {
            &edge.source
        };
        let Some(node) = found.get(end) else { continue };
        if nodes.contains(&node.id) {
            continue;
        }
        let next = node.id.clone();
        nodes.insert(node.clone());
        edges.push(edge);
        type_relatives(store, &next, ancestors, nodes, edges, visited)?;
    }
    Ok(())
}

/// Upstream `traverseBFS` from `start` (`graph/traversal.ts` 48-145) in both
/// directions over every edge kind, stepping onto [`HIGH_VALUE_KINDS`] only:
/// structural edges first (`contains`, then `calls`), each node queued once,
/// the node budget checked at every addition, and every distinct edge between
/// kept nodes recorded while scanning.
///
/// A neighbour neither visited nor queued when its visit began has its row
/// read through `reads`, in edge order, a batch at a time, and only while
/// the walk has room: past `limit` such a neighbour is skipped without its
/// row, as it was skipped after reading it.
fn breadth_first(
    store: &Store,
    start: &Node,
    max_depth: usize,
    limit: usize,
    reads: &mut WalkReads,
) -> anyhow::Result<(Vec<Node>, Vec<Edge>)> {
    let mut nodes = Gathered::default();
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen_edges: HashSet<EdgeIdentity> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut enqueued: HashSet<String> = HashSet::from([start.id.clone()]);
    let mut queue: VecDeque<(Node, usize)> = VecDeque::from([(start.clone(), 0)]);
    nodes.insert(start.clone());
    while nodes.len() < limit {
        let Some((node, depth)) = queue.pop_front() else {
            break;
        };
        if !visited.insert(node.id.clone()) || depth >= max_depth {
            continue;
        }
        let (mut adjacent, incoming) = reads.edges(store, &node.id)?;
        adjacent.extend(incoming);
        adjacent.sort_by_key(|edge| match edge.kind {
            EdgeKind::Contains => 0,
            EdgeKind::Calls => 1,
            _ => 2,
        });
        let wanted: Vec<String> = adjacent
            .iter()
            .map(|edge| other_end(edge, &node.id).to_string())
            .filter(|id| !visited.contains(id) && !enqueued.contains(id))
            .collect();
        let new_at_visit: HashSet<&str> = wanted.iter().map(String::as_str).collect();
        let mut next_unread = 0usize;
        let mut batch = FIRST_ROW_BATCH;
        for edge in adjacent {
            let next_id = other_end(&edge, &node.id).to_string();
            let new = new_at_visit.contains(next_id.as_str());
            if new && nodes.len() >= limit && !nodes.contains(&next_id) {
                continue;
            }
            if new && !reads.has_row(&next_id) {
                // This one and the next unread ones of the visit, in edge order.
                let mut ids: Vec<String> = vec![next_id.clone()];
                while next_unread < wanted.len() && ids.len() < batch {
                    let id = &wanted[next_unread];
                    next_unread += 1;
                    if !reads.has_row(id) && *id != next_id {
                        ids.push(id.clone());
                    }
                }
                batch *= 2;
                reads.read_rows(store, &ids)?;
            }
            let found = if new { reads.row(&next_id) } else { None };
            let Some(next) = found
                .or_else(|| nodes.index.get(&next_id).map(|at| &nodes.nodes[*at]))
                .cloned()
            else {
                continue;
            };
            if !HIGH_VALUE_KINDS.contains(&next.kind) {
                continue;
            }
            if !visited.contains(&next_id) && !enqueued.contains(&next_id) {
                if nodes.len() >= limit {
                    continue;
                }
                enqueued.insert(next_id);
                nodes.insert(next.clone());
                queue.push_back((next, depth + 1));
            }
            let key = (
                edge.source.clone(),
                edge.target.clone(),
                edge.kind,
                edge.line,
                edge.col,
            );
            if seen_edges.insert(key) {
                edges.push(edge);
            }
        }
    }
    Ok((nodes.nodes, edges))
}

fn other_end<'e>(edge: &'e Edge, from: &str) -> &'e str {
    if edge.source == from {
        &edge.target
    } else {
        &edge.source
    }
}

/// Past `maxNodes`, the entry points and the nodes one edge from the kept
/// set come first (the set grows while the edges are read once, in order),
/// then the rest in gather order (`context/index.ts` 1197-1233).
fn trim_to_budget(gathered: &mut Gathered, root_ids: &[String]) {
    if gathered.len() <= MAX_NODES {
        return;
    }
    let mut priority: Vec<String> = Vec::new();
    let mut in_priority: HashSet<String> = HashSet::new();
    for id in root_ids {
        if in_priority.insert(id.clone()) {
            priority.push(id.clone());
        }
    }
    for edge in &gathered.edges {
        if in_priority.contains(&edge.source) && in_priority.insert(edge.target.clone()) {
            priority.push(edge.target.clone());
        }
        if in_priority.contains(&edge.target) && in_priority.insert(edge.source.clone()) {
            priority.push(edge.source.clone());
        }
    }
    let mut keep: HashSet<String> = HashSet::new();
    for id in &priority {
        if keep.len() >= MAX_NODES {
            break;
        }
        if gathered.contains(id) {
            keep.insert(id.clone());
        }
    }
    for node in &gathered.nodes {
        if keep.len() >= MAX_NODES {
            break;
        }
        keep.insert(node.id.clone());
    }
    let order: HashMap<&str, usize> = priority
        .iter()
        .enumerate()
        .map(|(at, id)| (id.as_str(), at))
        .collect();
    // Upstream rebuilds its map in priority order, then fills the rest in
    // gather order, so the kept nodes come in that order too.
    let mut kept: Vec<Node> = gathered
        .nodes
        .iter()
        .filter(|node| keep.contains(&node.id))
        .cloned()
        .collect();
    kept.sort_by_key(|node| order.get(node.id.as_str()).copied().unwrap_or(usize::MAX));
    let edges = std::mem::take(&mut gathered.edges);
    *gathered = Gathered::default();
    for node in kept {
        gathered.insert(node);
    }
    for edge in edges {
        gathered.add_edge(edge);
    }
    gathered.retain_edges();
}

/// At most `max(5, ⌈maxNodes × 0.2⌉)` nodes of one file: entry points, then
/// type declarations, then callables, then the rest, each group in gather
/// order (`context/index.ts` 1235-1266).
fn cap_per_file(gathered: &mut Gathered, root_ids: &[String]) {
    let cap = 5usize.max((MAX_NODES as f64 * 0.2).ceil() as usize);
    let mut by_file: Vec<(String, Vec<usize>)> = Vec::new();
    let mut file_at: HashMap<String, usize> = HashMap::new();
    for (at, node) in gathered.nodes.iter().enumerate() {
        let slot = *file_at.entry(node.file_path.clone()).or_insert_with(|| {
            by_file.push((node.file_path.clone(), Vec::new()));
            by_file.len() - 1
        });
        by_file[slot].1.push(at);
    }
    let roots: HashSet<&str> = root_ids.iter().map(String::as_str).collect();
    let mut drop: HashSet<String> = HashSet::new();
    for (_, mut members) in by_file {
        if members.len() <= cap {
            continue;
        }
        let rank = |at: &usize| {
            let node = &gathered.nodes[*at];
            let root = if roots.contains(node.id.as_str()) {
                10
            } else {
                0
            };
            root + kind_priority(node.kind)
        };
        members.sort_by_key(|at| std::cmp::Reverse(rank(at)));
        for at in members.into_iter().skip(cap) {
            drop.insert(gathered.nodes[at].id.clone());
        }
    }
    if drop.is_empty() {
        return;
    }
    let keep: HashSet<String> = gathered
        .nodes
        .iter()
        .filter(|node| !drop.contains(&node.id))
        .map(|node| node.id.clone())
        .collect();
    gathered.retain(&keep);
}

/// Upstream's `kindPriority` for the per-file cap (`context/index.ts` 1252-1255).
fn kind_priority(kind: NodeKind) -> u32 {
    match kind {
        NodeKind::Class
        | NodeKind::Interface
        | NodeKind::Struct
        | NodeKind::Trait
        | NodeKind::Protocol
        | NodeKind::Enum => 3,
        NodeKind::Method | NodeKind::Function => 1,
        _ => 0,
    }
}

/// At most `max(3, ⌈maxNodes × 0.15⌉)` nodes of non-production files (tests,
/// examples, fixtures: the engine's wide test predicate), in gather order; an
/// entry point dropped here stops being one (`context/index.ts` 1267-1283).
fn cap_non_production(gathered: &mut Gathered, root_ids: &mut Vec<String>) {
    let cap = 3usize.max((MAX_NODES as f64 * 0.15).ceil() as usize);
    let non_production: Vec<String> = gathered
        .nodes
        .iter()
        .filter(|node| is_non_production_file(&node.file_path))
        .map(|node| node.id.clone())
        .collect();
    if non_production.len() <= cap {
        return;
    }
    let drop: HashSet<String> = non_production.into_iter().skip(cap).collect();
    root_ids.retain(|id| !drop.contains(id));
    let keep: HashSet<String> = gathered
        .nodes
        .iter()
        .filter(|node| !drop.contains(&node.id))
        .map(|node| node.id.clone())
        .collect();
    gathered.retain(&keep);
}

/// The edges out of `id` of `kinds` (all kinds when empty) in upstream's
/// `getOutgoingEdges` order: kind, target, line, column, a missing line or
/// column first as SQLite sorts NULL.
pub(crate) fn outgoing_in_order(
    store: &Store,
    id: &str,
    kinds: &[EdgeKind],
) -> anyhow::Result<Vec<Edge>> {
    let mut edges = edges_of(store, id, kinds, true)?;
    edges.sort_by(|a, b| {
        (a.kind.as_str(), a.target.as_str(), a.line, a.col).cmp(&(
            b.kind.as_str(),
            b.target.as_str(),
            b.line,
            b.col,
        ))
    });
    Ok(edges)
}

/// The edges into `id` of `kinds` (all kinds when empty) in upstream's
/// `getIncomingEdges` order: kind, source, line, column.
pub(crate) fn incoming_in_order(
    store: &Store,
    id: &str,
    kinds: &[EdgeKind],
) -> anyhow::Result<Vec<Edge>> {
    let mut edges = edges_of(store, id, kinds, false)?;
    edges.sort_by(|a, b| {
        (a.kind.as_str(), a.source.as_str(), a.line, a.col).cmp(&(
            b.kind.as_str(),
            b.source.as_str(),
            b.line,
            b.col,
        ))
    });
    Ok(edges)
}

fn edges_of(
    store: &Store,
    id: &str,
    kinds: &[EdgeKind],
    outgoing: bool,
) -> anyhow::Result<Vec<Edge>> {
    let read = |kind: Option<EdgeKind>| {
        if outgoing {
            store.edges_by_source_kind(id, kind)
        } else {
            store.edges_by_target_kind(id, kind)
        }
    };
    if kinds.is_empty() {
        return Ok(read(None)?);
    }
    let mut edges = Vec::new();
    for kind in kinds {
        edges.extend(read(Some(*kind))?);
    }
    Ok(edges)
}

/// Upstream `getCallers(id)` at its default depth of one: the distinct nodes
/// with a `calls`, `references`, `imports`, `instantiates` or `navigates` edge
/// into `id`, in edge order, `id` itself left out.
pub(crate) fn callers(store: &Store, id: &str) -> anyhow::Result<Vec<Node>> {
    let edges = incoming_in_order(store, id, &CALLER_KINDS)?;
    ends_in_order(store, id, edges.iter().map(|edge| edge.source.clone()))
}

/// Upstream `getCallees(id)` at depth one, the mirror of [`callers`].
pub(crate) fn callees(store: &Store, id: &str) -> anyhow::Result<Vec<Node>> {
    let edges = outgoing_in_order(store, id, &CALLER_KINDS)?;
    ends_in_order(store, id, edges.iter().map(|edge| edge.target.clone()))
}

fn ends_in_order(
    store: &Store,
    id: &str,
    ends: impl Iterator<Item = String>,
) -> anyhow::Result<Vec<Node>> {
    let order = distinct_ends(id, ends);
    let found = store.nodes_by_ids(&order)?;
    Ok(order
        .iter()
        .filter_map(|id| found.get(id).cloned())
        .collect())
}

/// The distinct `ends` in their first order, `id` itself left out.
fn distinct_ends(id: &str, ends: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([id.to_string()]);
    for end in ends {
        if seen.insert(end.clone()) {
            order.push(end);
        }
    }
    order
}

/// What the walk read from the store, by id, on this thread: the hook the
/// tests that pin "each node read once" count with (W7-25 cost).
#[cfg(test)]
pub(crate) mod test_reads {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static ROWS: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
        static EDGES: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
    }

    pub(crate) fn note_row(id: &str) {
        ROWS.with(|rows| *rows.borrow_mut().entry(id.to_string()).or_default() += 1);
    }

    pub(crate) fn note_edges(id: &str) {
        EDGES.with(|edges| *edges.borrow_mut().entry(id.to_string()).or_default() += 1);
    }

    /// The counts since the last call, `(rows, edges)`: how often each id's
    /// row and each id's edges were read.
    pub(crate) fn take() -> (HashMap<String, usize>, HashMap<String, usize>) {
        (
            ROWS.with(|rows| std::mem::take(&mut *rows.borrow_mut())),
            EDGES.with(|edges| std::mem::take(&mut *edges.borrow_mut())),
        )
    }
}
