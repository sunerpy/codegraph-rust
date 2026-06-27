//! Graph traversal over the store's resolved edge graph.
//!
//! Ports `upstream graph/traversal.ts` (`GraphTraverser`) and the
//! `findAllSymbols`/`getNodesByName` ambiguity path that `codegraph_node`
//! consumes (`upstream mcp/tools.ts:3220-3307`). Sync over rusqlite —
//! there is no async in this layer. Kept in its own module to avoid colliding
//! with the committed search `query` module (Task 21).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;
use serde::Serialize;

// upstream graph/traversal.ts:506 — container kinds whose `contains`
// children are pulled into the impact set at the same depth as the container.
const CONTAINER_KINDS: [NodeKind; 7] = [
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Struct,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Module,
    NodeKind::Enum,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone)]
pub struct TraversalOptions {
    pub max_depth: Option<usize>,
    pub edge_kinds: Vec<EdgeKind>,
    pub node_kinds: Vec<NodeKind>,
    pub direction: Direction,
    pub limit: usize,
    pub include_start: bool,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            edge_kinds: Vec::new(),
            node_kinds: Vec::new(),
            direction: Direction::Outgoing,
            limit: 1000,
            include_start: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subgraph {
    pub nodes: HashMap<String, Node>,
    pub node_order: Vec<String>,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
}

impl Subgraph {
    fn empty() -> Self {
        Self {
            nodes: HashMap::new(),
            node_order: Vec::new(),
            edges: Vec::new(),
            roots: Vec::new(),
        }
    }

    fn set_node(&mut self, node: Node) {
        if !self.nodes.contains_key(&node.id) {
            self.node_order.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    pub fn ordered_nodes(&self) -> Vec<&Node> {
        self.node_order
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct NodeEdge {
    pub node: Node,
    pub edge: Edge,
}

#[derive(Debug, Clone)]
pub struct PathStep {
    pub node: Node,
    pub edge: Option<Edge>,
}

/// Sentinel-prefix that L3's `godot_script` resolver stamps onto the
/// `reference_name` of a computed (statically-unconfirmable) Godot call-site.
/// Mirrors `codegraph_resolve::frameworks::godot_script::DYNAMIC_PREFIX`;
/// duplicated here because `codegraph-graph` does not depend on the resolve
/// crate, and a single source-level constant cannot be shared without that dep.
const GODOT_DYNAMIC_PREFIX: &str = "godot:dynamic:";

/// Honesty signal for the caller/dead-code surfaces: whether a symbol with no
/// static caller is nonetheless reached through a Godot dynamic/structural link,
/// plus the computed call-sites it owns that cannot be statically confirmed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GodotDynamicReach {
    /// Reasons the symbol is reachable at runtime despite zero static callers.
    /// Empty when no Godot link targets the symbol.
    pub reached_by: Vec<GodotReach>,
    /// `godot:dynamic:` sentinel reference names ORIGINATING from this symbol —
    /// computed call-sites whose target the static analysis cannot pin down.
    pub dynamic_unresolved: Vec<String>,
}

/// One way a symbol is reached through a Godot runtime link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GodotReach {
    /// A `.tscn`/`.tres`/`project.godot` reference name-matched the symbol.
    SceneOrResourceLink,
    /// The symbol's file is the script bound to an autoload singleton.
    Autoload,
}

impl GodotDynamicReach {
    pub fn is_dynamically_reachable(&self) -> bool {
        !self.reached_by.is_empty()
    }

    pub fn has_any_signal(&self) -> bool {
        !self.reached_by.is_empty() || !self.dynamic_unresolved.is_empty()
    }
}

/// A Godot `.tres`/`.tscn` resource file no indexed reference names. Keyed on
/// the repo-relative path (resource files have no `file:` graph node — orphan
/// accounting is by path, per the B0 probe finding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanResource {
    pub file_path: String,
}

/// A path-shaped Godot reference whose target is missing on disk under the
/// project root and is not an excluded (`.godot/`/`addons/`/`godot:dynamic:`) ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DanglingRef {
    pub from_file: String,
    pub target_path: String,
    pub line: i64,
    pub kind: String,
}

/// The reverse-dependency view for one changed resource/script path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceImpact {
    pub changed: String,
    pub affected: Vec<AffectedRef>,
}

/// One referencing site (source file + line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedRef {
    pub from_file: String,
    pub line: i64,
}

pub struct GraphTraverser<'store> {
    store: &'store Store,
}

impl<'store> GraphTraverser<'store> {
    pub fn new(store: &'store Store) -> Self {
        Self { store }
    }

    /// Ports `traverseBFS` from `upstream graph/traversal.ts:48-120`.
    /// Cycle-safe via the `visited` set; honours `max_depth`/`limit`; orders the
    /// frontier structural-edges-first (`contains` < `calls` < other) so internal
    /// structure is discovered before fanning out to references.
    pub fn traverse_bfs(
        &self,
        start_id: &str,
        options: &TraversalOptions,
    ) -> rusqlite::Result<Subgraph> {
        let Some(start_node) = self.store.node_by_id(start_id)? else {
            return Ok(Subgraph::empty());
        };

        let mut graph = Subgraph::empty();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(Node, Option<Edge>, usize)> = VecDeque::new();
        queue.push_back((start_node.clone(), None, 0));

        if options.include_start {
            graph.set_node(start_node);
        }

        while let Some((node, edge, depth)) = queue.pop_front() {
            if graph.nodes.len() >= options.limit {
                break;
            }
            if visited.contains(&node.id) {
                continue;
            }
            visited.insert(node.id.clone());

            if let Some(edge) = edge {
                graph.edges.push(edge);
            }

            if depth_reached(depth, options.max_depth) {
                continue;
            }

            let mut adjacent =
                self.adjacent_edges(&node.id, options.direction, &options.edge_kinds)?;
            adjacent.sort_by_key(structural_priority);

            let want_ids = unvisited_neighbor_ids(&adjacent, &node.id, &visited);
            let neighbor_nodes = self.store.nodes_by_ids(&want_ids)?;

            for adj_edge in &adjacent {
                let next_id = neighbor_id(adj_edge, &node.id);
                if visited.contains(next_id) {
                    continue;
                }
                let Some(next_node) = neighbor_nodes.get(next_id) else {
                    continue;
                };
                if !options.node_kinds.is_empty() && !options.node_kinds.contains(&next_node.kind) {
                    continue;
                }
                graph.set_node(next_node.clone());
                queue.push_back((next_node.clone(), Some(adj_edge.clone()), depth + 1));
            }
        }

        graph.roots = vec![start_id.to_string()];
        Ok(graph)
    }

    /// Ports `traverseDFS` from `upstream graph/traversal.ts:129-199`.
    pub fn traverse_dfs(
        &self,
        start_id: &str,
        options: &TraversalOptions,
    ) -> rusqlite::Result<Subgraph> {
        let Some(start_node) = self.store.node_by_id(start_id)? else {
            return Ok(Subgraph::empty());
        };

        let mut graph = Subgraph::empty();
        let mut visited: HashSet<String> = HashSet::new();

        if options.include_start {
            graph.set_node(start_node.clone());
        }

        self.dfs_recursive(&start_node, 0, options, &mut graph, &mut visited)?;
        graph.roots = vec![start_id.to_string()];
        Ok(graph)
    }

    fn dfs_recursive(
        &self,
        node: &Node,
        depth: usize,
        options: &TraversalOptions,
        graph: &mut Subgraph,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if visited.contains(&node.id)
            || graph.nodes.len() >= options.limit
            || depth_reached(depth, options.max_depth)
        {
            return Ok(());
        }
        visited.insert(node.id.clone());

        let adjacent = self.adjacent_edges(&node.id, options.direction, &options.edge_kinds)?;
        let want_ids = unvisited_neighbor_ids(&adjacent, &node.id, visited);
        let neighbor_nodes = self.store.nodes_by_ids(&want_ids)?;

        for edge in &adjacent {
            let next_id = neighbor_id(edge, &node.id);
            if visited.contains(next_id) {
                continue;
            }
            let Some(next_node) = neighbor_nodes.get(next_id) else {
                continue;
            };
            if !options.node_kinds.is_empty() && !options.node_kinds.contains(&next_node.kind) {
                continue;
            }
            graph.set_node(next_node.clone());
            graph.edges.push(edge.clone());
            self.dfs_recursive(next_node, depth + 1, options, graph, visited)?;
        }
        Ok(())
    }

    fn adjacent_edges(
        &self,
        node_id: &str,
        direction: Direction,
        edge_kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        match direction {
            Direction::Outgoing => self.outgoing_edges(node_id, edge_kinds),
            Direction::Incoming => self.incoming_edges(node_id, edge_kinds),
            Direction::Both => {
                let mut outgoing = self.outgoing_edges(node_id, edge_kinds)?;
                let incoming = self.incoming_edges(node_id, edge_kinds)?;
                outgoing.extend(incoming);
                Ok(outgoing)
            }
        }
    }

    /// Ports `getCallers` from `upstream graph/traversal.ts:230-266`.
    /// Incoming `calls`/`references`/`imports` edges, recursive to `max_depth`,
    /// cycle-safe via `visited`.
    pub fn get_callers(&self, node_id: &str, max_depth: usize) -> rusqlite::Result<Vec<NodeEdge>> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        self.callers_recursive(node_id, max_depth, 0, &mut result, &mut visited)?;
        Ok(result)
    }

    fn callers_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        current_depth: usize,
        result: &mut Vec<NodeEdge>,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if current_depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let incoming = self.incoming_edges_kinds(
            node_id,
            &[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports],
        )?;
        if incoming.is_empty() {
            return Ok(());
        }

        let source_ids: Vec<String> = incoming.iter().map(|e| e.source.clone()).collect();
        let caller_nodes = self.store.nodes_by_ids(&source_ids)?;

        for edge in incoming {
            if let Some(caller) = caller_nodes.get(&edge.source) {
                if !visited.contains(&caller.id) {
                    let caller = caller.clone();
                    let caller_id = caller.id.clone();
                    result.push(NodeEdge { node: caller, edge });
                    self.callers_recursive(
                        &caller_id,
                        max_depth,
                        current_depth + 1,
                        result,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// T8 (L6) honesty signal: does a Godot dynamic/structural link reach this
    /// symbol, and which `godot:dynamic:` sentinel sites does it own?
    ///
    /// Two reachability signals (either is sufficient):
    /// 1. an unresolved reference whose `reference_name` equals the symbol's
    ///    name AND originates from a `.tscn`/`.tres`/`project.godot` file — the
    ///    Godot engine wires the target at runtime (`[connection]` handler,
    ///    scene/resource script binding, group), so name-match is the honest
    ///    link the static graph cannot turn into an edge;
    /// 2. an autoload singleton (`project.godot` `Constant`) whose post-extract
    ///    `signature` (`autoload -> <path>`) names this symbol's file.
    ///
    /// The result is empty for any symbol with no such Godot link — so the
    /// caller/dead-code surfaces stay byte-unchanged for non-Godot projects.
    pub fn godot_dynamic_reachability(&self, node: &Node) -> rusqlite::Result<GodotDynamicReach> {
        let mut reached_by = Vec::new();

        let name_links = self
            .store
            .unresolved_refs_by_names(std::slice::from_ref(&node.name))?;
        if name_links
            .iter()
            .any(|r| r.language.is_godot_non_script_file())
        {
            reached_by.push(GodotReach::SceneOrResourceLink);
        }

        if self.file_is_autoload_bound(&node.file_path)? {
            reached_by.push(GodotReach::Autoload);
        }

        let mut dynamic_unresolved: Vec<String> = self
            .store
            .all_unresolved_refs()?
            .into_iter()
            .filter(|r| {
                r.from_node_id == node.id && r.reference_name.starts_with(GODOT_DYNAMIC_PREFIX)
            })
            .map(|r| r.reference_name)
            .collect();
        dynamic_unresolved.sort();
        dynamic_unresolved.dedup();

        Ok(GodotDynamicReach {
            reached_by,
            dynamic_unresolved,
        })
    }

    fn file_is_autoload_bound(&self, file_path: &str) -> rusqlite::Result<bool> {
        let binding = format!("autoload -> {file_path}");
        Ok(self
            .store
            .nodes_by_kind(NodeKind::Constant)?
            .into_iter()
            .any(|n| {
                n.language == Language::GodotProject
                    && n.signature.as_deref() == Some(binding.as_str())
            }))
    }

    /// Ports `getCallees` from `upstream graph/traversal.ts:275-310`.
    /// Outgoing `calls`/`references`/`imports` edges, recursive to `max_depth`,
    /// cycle-safe via `visited`.
    pub fn get_callees(&self, node_id: &str, max_depth: usize) -> rusqlite::Result<Vec<NodeEdge>> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        self.callees_recursive(node_id, max_depth, 0, &mut result, &mut visited)?;
        Ok(result)
    }

    fn callees_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        current_depth: usize,
        result: &mut Vec<NodeEdge>,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if current_depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let outgoing = self.outgoing_edges_kinds(
            node_id,
            &[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports],
        )?;
        if outgoing.is_empty() {
            return Ok(());
        }

        let target_ids: Vec<String> = outgoing.iter().map(|e| e.target.clone()).collect();
        let callee_nodes = self.store.nodes_by_ids(&target_ids)?;

        for edge in outgoing {
            if let Some(callee) = callee_nodes.get(&edge.target) {
                if !visited.contains(&callee.id) {
                    let callee = callee.clone();
                    let callee_id = callee.id.clone();
                    result.push(NodeEdge { node: callee, edge });
                    self.callees_recursive(
                        &callee_id,
                        max_depth,
                        current_depth + 1,
                        result,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Ports `getCallGraph` from `upstream graph/traversal.ts:319-350`.
    pub fn get_call_graph(&self, node_id: &str, depth: usize) -> rusqlite::Result<Subgraph> {
        let Some(focal) = self.store.node_by_id(node_id)? else {
            return Ok(Subgraph::empty());
        };

        let mut graph = Subgraph::empty();
        graph.set_node(focal);

        for entry in self.get_callers(node_id, depth)? {
            graph.set_node(entry.node);
            graph.edges.push(entry.edge);
        }
        for entry in self.get_callees(node_id, depth)? {
            graph.set_node(entry.node);
            graph.edges.push(entry.edge);
        }

        graph.roots = vec![node_id.to_string()];
        Ok(graph)
    }

    /// Ports `getTypeHierarchy` from `upstream graph/traversal.ts:358-382`.
    pub fn get_type_hierarchy(&self, node_id: &str) -> rusqlite::Result<Subgraph> {
        let Some(focal) = self.store.node_by_id(node_id)? else {
            return Ok(Subgraph::empty());
        };

        let mut graph = Subgraph::empty();
        let mut visited = HashSet::new();
        graph.set_node(focal);

        self.type_ancestors(node_id, &mut graph, &mut visited)?;
        self.type_descendants(node_id, &mut graph, &mut visited)?;

        graph.roots = vec![node_id.to_string()];
        Ok(graph)
    }

    fn type_ancestors(
        &self,
        node_id: &str,
        graph: &mut Subgraph,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let outgoing =
            self.outgoing_edges_kinds(node_id, &[EdgeKind::Extends, EdgeKind::Implements])?;
        if outgoing.is_empty() {
            return Ok(());
        }
        let parents = self.store.nodes_by_ids(
            &outgoing
                .iter()
                .map(|e| e.target.clone())
                .collect::<Vec<_>>(),
        )?;

        for edge in outgoing {
            if let Some(parent) = parents.get(&edge.target) {
                if !graph.nodes.contains_key(&parent.id) {
                    let parent = parent.clone();
                    let parent_id = parent.id.clone();
                    graph.set_node(parent);
                    graph.edges.push(edge);
                    self.type_ancestors(&parent_id, graph, visited)?;
                }
            }
        }
        Ok(())
    }

    fn type_descendants(
        &self,
        node_id: &str,
        graph: &mut Subgraph,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let incoming =
            self.incoming_edges_kinds(node_id, &[EdgeKind::Extends, EdgeKind::Implements])?;
        if incoming.is_empty() {
            return Ok(());
        }
        let children = self.store.nodes_by_ids(
            &incoming
                .iter()
                .map(|e| e.source.clone())
                .collect::<Vec<_>>(),
        )?;

        for edge in incoming {
            if let Some(child) = children.get(&edge.source) {
                if !graph.nodes.contains_key(&child.id) {
                    let child = child.clone();
                    let child_id = child.id.clone();
                    graph.set_node(child);
                    graph.edges.push(edge);
                    self.type_descendants(&child_id, graph, visited)?;
                }
            }
        }
        Ok(())
    }

    /// Ports `findUsages` from `upstream graph/traversal.ts:440-455`.
    pub fn find_usages(&self, node_id: &str) -> rusqlite::Result<Vec<NodeEdge>> {
        let mut result = Vec::new();
        let incoming = self.incoming_edges(node_id, &[])?;
        if incoming.is_empty() {
            return Ok(result);
        }
        let sources = self.store.nodes_by_ids(
            &incoming
                .iter()
                .map(|e| e.source.clone())
                .collect::<Vec<_>>(),
        )?;
        for edge in incoming {
            if let Some(source) = sources.get(&edge.source) {
                result.push(NodeEdge {
                    node: source.clone(),
                    edge,
                });
            }
        }
        Ok(result)
    }

    /// Ports `getImpactRadius` from `upstream graph/traversal.ts:466-540`.
    /// Transitive blast radius over INCOMING dependents (excluding `contains`,
    /// per #536); container symbols also pull their `contains` children in at the
    /// same depth. Cycle-safe via `visited`; bounded by `max_depth`.
    pub fn get_impact_radius(&self, node_id: &str, max_depth: usize) -> rusqlite::Result<Subgraph> {
        let Some(focal) = self.store.node_by_id(node_id)? else {
            return Ok(Subgraph::empty());
        };

        let mut graph = Subgraph::empty();
        let mut visited = HashSet::new();
        graph.set_node(focal);

        self.impact_recursive(node_id, max_depth, 0, &mut graph, &mut visited)?;

        graph.roots = vec![node_id.to_string()];
        Ok(graph)
    }

    fn impact_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        current_depth: usize,
        graph: &mut Subgraph,
        visited: &mut HashSet<String>,
    ) -> rusqlite::Result<()> {
        if current_depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        if let Some(focal) = self.store.node_by_id(node_id)? {
            if CONTAINER_KINDS.contains(&focal.kind) {
                let contains = self.outgoing_edges_kinds(node_id, &[EdgeKind::Contains])?;
                if !contains.is_empty() {
                    let children = self.store.nodes_by_ids(
                        &contains
                            .iter()
                            .map(|e| e.target.clone())
                            .collect::<Vec<_>>(),
                    )?;
                    for edge in contains {
                        if let Some(child) = children.get(&edge.target) {
                            if !visited.contains(&child.id) {
                                let child = child.clone();
                                let child_id = child.id.clone();
                                graph.set_node(child);
                                graph.edges.push(edge);
                                self.impact_recursive(
                                    &child_id,
                                    max_depth,
                                    current_depth,
                                    graph,
                                    visited,
                                )?;
                            }
                        }
                    }
                }
            }
        }

        let incoming: Vec<Edge> = self
            .incoming_edges(node_id, &[])?
            .into_iter()
            .filter(|e| e.kind != EdgeKind::Contains)
            .collect();
        if incoming.is_empty() {
            return Ok(());
        }
        let sources = self.store.nodes_by_ids(
            &incoming
                .iter()
                .map(|e| e.source.clone())
                .collect::<Vec<_>>(),
        )?;

        for edge in incoming {
            if let Some(source) = sources.get(&edge.source) {
                if !graph.nodes.contains_key(&source.id) {
                    let source = source.clone();
                    let source_id = source.id.clone();
                    graph.set_node(source);
                    graph.edges.push(edge);
                    self.impact_recursive(
                        &source_id,
                        max_depth,
                        current_depth + 1,
                        graph,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Ports `findPath` from `upstream graph/traversal.ts:550-607`.
    /// BFS shortest path over outgoing edges; cycle-safe via `visited`.
    pub fn find_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
    ) -> rusqlite::Result<Option<Vec<PathStep>>> {
        let Some(from_node) = self.store.node_by_id(from_id)? else {
            return Ok(None);
        };
        if self.store.node_by_id(to_id)?.is_none() {
            return Ok(None);
        }

        let mut visited = HashSet::new();
        let mut queue: VecDeque<(String, Vec<PathStep>)> = VecDeque::new();
        queue.push_back((
            from_id.to_string(),
            vec![PathStep {
                node: from_node,
                edge: None,
            }],
        ));

        while let Some((node_id, path)) = queue.pop_front() {
            if node_id == to_id {
                return Ok(Some(path));
            }
            if visited.contains(&node_id) {
                continue;
            }
            visited.insert(node_id.clone());

            let outgoing = self.outgoing_edges(&node_id, edge_kinds)?;
            if outgoing.is_empty() {
                continue;
            }
            let want_ids: Vec<String> = outgoing
                .iter()
                .map(|e| e.target.clone())
                .filter(|id| !visited.contains(id))
                .collect();
            let next_nodes = self.store.nodes_by_ids(&want_ids)?;

            for edge in outgoing {
                if !visited.contains(&edge.target) {
                    if let Some(next_node) = next_nodes.get(&edge.target) {
                        let mut next_path = path.clone();
                        next_path.push(PathStep {
                            node: next_node.clone(),
                            edge: Some(edge.clone()),
                        });
                        queue.push_back((edge.target.clone(), next_path));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Ports `getAncestors` from `upstream graph/traversal.ts:615-645`.
    pub fn get_ancestors(&self, node_id: &str) -> rusqlite::Result<Vec<Node>> {
        let mut ancestors = Vec::new();
        let mut visited = HashSet::new();
        let mut current_id = node_id.to_string();

        loop {
            if visited.contains(&current_id) {
                break;
            }
            visited.insert(current_id.clone());

            let containing = self.incoming_edges_kinds(&current_id, &[EdgeKind::Contains])?;
            let Some(first_edge) = containing.into_iter().next() else {
                break;
            };
            match self.store.node_by_id(&first_edge.source)? {
                Some(parent) => {
                    current_id = parent.id.clone();
                    ancestors.push(parent);
                }
                None => break,
            }
        }

        Ok(ancestors)
    }

    /// Ports `findCircularDependencies` from
    /// `upstream graph/queries.ts:225-263`: DFS over the forward
    /// file-dependency graph (`dependency_file_paths` adjacency) with a
    /// `visited` set + `recursion_stack`; on revisiting a node already on the
    /// stack the cycle is sliced from `path` at the cycle start. Files are
    /// iterated in sorted order (the upstream uses SQLite `ORDER BY path`) so output is
    /// deterministic.
    pub fn find_circular_dependencies(&self) -> rusqlite::Result<Vec<Vec<String>>> {
        let mut files: Vec<String> = self
            .store
            .all_files()?
            .into_iter()
            .map(|f| f.path)
            .collect();
        files.sort();

        let mut cycles: Vec<Vec<String>> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut recursion_stack: HashSet<String> = HashSet::new();

        for file in &files {
            if !visited.contains(file) {
                self.cycle_dfs(
                    file,
                    &mut Vec::new(),
                    &mut visited,
                    &mut recursion_stack,
                    &mut cycles,
                )?;
            }
        }

        Ok(cycles)
    }

    fn cycle_dfs(
        &self,
        file_path: &str,
        path: &mut Vec<String>,
        visited: &mut HashSet<String>,
        recursion_stack: &mut HashSet<String>,
        cycles: &mut Vec<Vec<String>>,
    ) -> rusqlite::Result<()> {
        if recursion_stack.contains(file_path) {
            if let Some(cycle_start) = path.iter().position(|p| p == file_path) {
                cycles.push(path[cycle_start..].to_vec());
            }
            return Ok(());
        }
        if visited.contains(file_path) {
            return Ok(());
        }

        visited.insert(file_path.to_string());
        recursion_stack.insert(file_path.to_string());

        let dependencies = self.store.dependency_file_paths(file_path)?;
        path.push(file_path.to_string());
        for dep in dependencies {
            self.cycle_dfs(&dep, path, visited, recursion_stack, cycles)?;
        }
        path.pop();

        recursion_stack.remove(file_path);
        Ok(())
    }

    /// Ports `getChildren` from `upstream graph/traversal.ts:653-665`.
    pub fn get_children(&self, node_id: &str) -> rusqlite::Result<Vec<Node>> {
        let contains = self.outgoing_edges_kinds(node_id, &[EdgeKind::Contains])?;
        if contains.is_empty() {
            return Ok(Vec::new());
        }
        let child_nodes = self.store.nodes_by_ids(
            &contains
                .iter()
                .map(|e| e.target.clone())
                .collect::<Vec<_>>(),
        )?;
        let mut children = Vec::new();
        for edge in contains {
            if let Some(child) = child_nodes.get(&edge.target) {
                children.push(child.clone());
            }
        }
        Ok(children)
    }

    fn outgoing_edges(&self, node_id: &str, kinds: &[EdgeKind]) -> rusqlite::Result<Vec<Edge>> {
        if kinds.is_empty() {
            self.store.edges_by_source_kind(node_id, None)
        } else {
            self.outgoing_edges_kinds(node_id, kinds)
        }
    }

    fn incoming_edges(&self, node_id: &str, kinds: &[EdgeKind]) -> rusqlite::Result<Vec<Edge>> {
        if kinds.is_empty() {
            self.store.edges_by_target_kind(node_id, None)
        } else {
            self.incoming_edges_kinds(node_id, kinds)
        }
    }

    fn outgoing_edges_kinds(
        &self,
        node_id: &str,
        kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        let mut out = Vec::new();
        for kind in kinds {
            out.extend(self.store.edges_by_source_kind(node_id, Some(*kind))?);
        }
        Ok(out)
    }

    fn incoming_edges_kinds(
        &self,
        node_id: &str,
        kinds: &[EdgeKind],
    ) -> rusqlite::Result<Vec<Edge>> {
        let mut out = Vec::new();
        for kind in kinds {
            out.extend(self.store.edges_by_target_kind(node_id, Some(*kind))?);
        }
        Ok(out)
    }

    /// Godot `.tres`/`.tscn` resource files no indexed reference names (orphans).
    ///
    /// Read-only, golden-neutral. Per the B0 probe, godot resource files carry no
    /// `file:` graph node and their inbound references stay in `unresolved_refs`,
    /// so orphan accounting compares each resource's repo-relative path against
    /// the set of referenced paths (`unresolved_refs.reference_name` plus any
    /// resolved-edge target paths). Deterministic: sorted by `file_path`.
    pub fn find_orphan_resources(&self) -> rusqlite::Result<Vec<OrphanResource>> {
        let referenced = self.referenced_resource_paths()?;
        let mut orphans: Vec<OrphanResource> = self
            .store
            .all_files()?
            .into_iter()
            .filter(|f| {
                f.language.is_godot_non_script_file() && f.language != Language::GodotProject
            })
            .filter(|f| !referenced.contains(&normalize_rel(&f.path)))
            .map(|f| OrphanResource { file_path: f.path })
            .collect();
        orphans.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        Ok(orphans)
    }

    /// Path-shaped Godot references whose target is missing on disk (dangling).
    ///
    /// Read-only, golden-neutral. Source set is `all_unresolved_refs()` filtered
    /// to path-shaped names. Exclusion precedence: (1) normalized target prefix
    /// `.godot/`/`addons/` → skip; (2) `reference_name` prefix `godot:dynamic:` →
    /// skip; (3) survivors are dangling iff `project_root.join(normalized)` does
    /// not exist on disk. Deterministic: sorted by `(from_file, target_path, line)`.
    pub fn find_dangling_references(
        &self,
        project_root: &Path,
    ) -> rusqlite::Result<Vec<DanglingRef>> {
        let mut out: Vec<DanglingRef> = Vec::new();
        for reference in self.store.all_unresolved_refs()? {
            if !is_path_shaped(&reference.reference_name, reference.language) {
                continue;
            }
            let normalized = normalize_rel(&reference.reference_name);
            if is_excluded_prefix(&normalized) {
                continue;
            }
            if reference.reference_name.starts_with("godot:dynamic:") {
                continue;
            }
            if project_root.join(&normalized).exists() {
                continue;
            }
            out.push(DanglingRef {
                from_file: reference.file_path,
                target_path: normalized,
                line: reference.line,
                kind: reference.reference_kind.as_str().to_string(),
            });
        }
        out.sort_by(|a, b| {
            a.from_file
                .cmp(&b.from_file)
                .then_with(|| a.target_path.cmp(&b.target_path))
                .then_with(|| a.line.cmp(&b.line))
        });
        Ok(out)
    }

    /// Reverse-dependency (impact) list for a changed resource/script path.
    ///
    /// Read-only, golden-neutral. Lists every reference whose normalized target
    /// equals `changed_rel_path`: unresolved path refs (the godot reference home,
    /// per B0) plus any resolved incoming edges on the changed file's `file:`
    /// node (present for `.gd`/grammar files). Deterministic: sorted by
    /// `(from_file, line)`.
    pub fn resource_impact(&self, changed_rel_path: &str) -> rusqlite::Result<ResourceImpact> {
        let changed = normalize_rel(changed_rel_path);
        let mut affected: Vec<AffectedRef> = Vec::new();

        for reference in self.store.all_unresolved_refs()? {
            if normalize_rel(&reference.reference_name) == changed {
                affected.push(AffectedRef {
                    from_file: reference.file_path,
                    line: reference.line,
                });
            }
        }

        let file_id = codegraph_core::node_id::file_node_id(&changed);
        for edge in self.store.edges_by_target_kind(&file_id, None)? {
            if !matches!(edge.kind, EdgeKind::References | EdgeKind::Instantiates) {
                continue;
            }
            if let Some(source) = self.store.node_by_id(&edge.source)? {
                affected.push(AffectedRef {
                    from_file: source.file_path,
                    line: edge.line.unwrap_or(0),
                });
            }
        }

        affected.sort_by(|a, b| {
            a.from_file
                .cmp(&b.from_file)
                .then_with(|| a.line.cmp(&b.line))
        });
        affected.dedup();
        Ok(ResourceImpact { changed, affected })
    }

    /// The set of repo-relative resource paths referenced anywhere in the graph:
    /// every path-shaped `unresolved_refs.reference_name` plus the file path of
    /// any node that is the target of a resolved `References`/`Instantiates` edge.
    fn referenced_resource_paths(&self) -> rusqlite::Result<HashSet<String>> {
        let mut referenced: HashSet<String> = HashSet::new();
        for reference in self.store.all_unresolved_refs()? {
            if is_path_shaped(&reference.reference_name, reference.language) {
                referenced.insert(normalize_rel(&reference.reference_name));
            }
        }
        for node in self.store.nodes_by_kind(NodeKind::File)? {
            for kind in [EdgeKind::References, EdgeKind::Instantiates] {
                if !self
                    .store
                    .edges_by_target_kind(&node.id, Some(kind))?
                    .is_empty()
                {
                    referenced.insert(normalize_rel(&node.file_path));
                    break;
                }
            }
        }
        Ok(referenced)
    }
}

/// Normalize a stored relative path to `/`-separated form (paths are stored
/// `/`-joined by the node-id formula; this is belt-and-suspenders for any ref
/// carrying a backslash).
fn normalize_rel(path: &str) -> String {
    path.replace('\\', "/")
}

/// `.godot/` and `addons/` references are engine-managed / third-party and are
/// never reported as dangling, regardless of disk state.
fn is_excluded_prefix(normalized: &str) -> bool {
    normalized.starts_with(".godot/") || normalized.starts_with("addons/")
}

/// A reference name is path-shaped when it contains `/` AND ends in a Godot
/// resource extension, OR its language is a Godot non-script file language.
fn is_path_shaped(reference_name: &str, language: Language) -> bool {
    let by_language = matches!(
        language,
        Language::GodotScene | Language::GodotResource | Language::GodotProject
    );
    let by_extension = reference_name.contains('/')
        && (reference_name.ends_with(".tres")
            || reference_name.ends_with(".tscn")
            || reference_name.ends_with(".gd")
            || reference_name.ends_with(".res"));
    by_language || by_extension
}

/// Ambiguity resolution for `codegraph_node`: a bare name maps to EVERY exact
/// definition so all overloads are returned, matching
/// `upstream mcp/tools.ts:3229-3232` (`getNodesByName` exact-name
/// enumeration). Caller-side file/line disambiguation stays in the MCP layer
/// (Task 22); this primitive returns the full set.
pub fn find_all_definitions(store: &Store, name: &str) -> rusqlite::Result<Vec<Node>> {
    store.nodes_by_name(name)
}

/// upstream graph/traversal.ts:88-91 — frontier ordering priority
/// `contains` < `calls` < everything else.
fn structural_priority(edge: &Edge) -> u8 {
    match edge.kind {
        EdgeKind::Contains => 0,
        EdgeKind::Calls => 1,
        _ => 2,
    }
}

fn depth_reached(depth: usize, max_depth: Option<usize>) -> bool {
    max_depth.is_some_and(|max| depth >= max)
}

fn neighbor_id<'edge>(edge: &'edge Edge, node_id: &str) -> &'edge str {
    if edge.source == node_id {
        &edge.target
    } else {
        &edge.source
    }
}

fn unvisited_neighbor_ids(edges: &[Edge], node_id: &str, visited: &HashSet<String>) -> Vec<String> {
    edges
        .iter()
        .map(|e| neighbor_id(e, node_id).to_string())
        .filter(|id| !visited.contains(id))
        .collect()
}
