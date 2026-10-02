//! A type's supertypes and subtypes as the viewer draws them — a port of
//! upstream `src/graph/type-hierarchy.ts` (`buildTypeHierarchy`, `v1.6.1`).
//!
//! Supertypes walk `extends`/`implements` edges outward from the focus (nearest
//! first), subtypes walk them inward breadth-first so depth 1 is complete before
//! depth 2 starts, and both are bounded. Members of the focus that redeclare an
//! ancestor's member by name are marked as overrides — a name match inside a
//! chain the graph already links, not an `overrides` edge.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_store::Store;

/// Node kinds a hierarchy can hang off.
pub fn can_have_hierarchy(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Union
    )
}

fn is_overridable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Property | NodeKind::Field
    )
}

pub const MAX_ANCESTOR_DEPTH: usize = 8;
pub const MAX_DESCENDANT_DEPTH: usize = 6;
pub const MAX_DESCENDANTS: usize = 400;
const MAX_OVERRIDE_ANCESTORS: usize = 12;
/// Direct implementers at which a call through the type is "polymorphic".
pub const DISPATCH_MIN_IMPLEMENTERS: usize = 8;

/// How a subtype is tied to its supertype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyRelation {
    Extends,
    Implements,
}

impl HierarchyRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extends => "extends",
            Self::Implements => "implements",
        }
    }

    fn of(edge: &Edge) -> Self {
        if edge.kind == EdgeKind::Implements {
            Self::Implements
        } else {
            Self::Extends
        }
    }
}

#[derive(Debug, Clone)]
pub struct HierarchyEntry {
    pub node: Node,
    /// Steps from the focus; 1 = declared directly on the focus.
    pub depth: usize,
    /// The entry one step nearer the focus (the focus's own id at depth 1).
    pub parent_id: String,
    pub relation: HierarchyRelation,
    /// The edge, oriented subtype → supertype as the code declares it.
    pub edge: Edge,
    /// Synthesized rather than parsed (provenance `heuristic`).
    pub synthesized: bool,
    /// Direct subtypes this entry has that are NOT in the returned set.
    pub hidden_subtypes: usize,
}

#[derive(Debug, Clone)]
pub struct OverrideMatch {
    pub member_id: String,
    pub base_id: String,
    pub base_type_id: String,
    pub base_type_name: String,
    pub relation: HierarchyRelation,
}

#[derive(Debug, Clone)]
pub struct TypeHierarchy {
    pub focus: Node,
    /// Supertypes, nearest first.
    pub ancestors: Vec<HierarchyEntry>,
    /// Subtypes, breadth-first.
    pub descendants: Vec<HierarchyEntry>,
    pub direct_subtypes: usize,
    pub direct_implementers: usize,
    pub bounded: bool,
    pub polymorphic: bool,
    /// Members of the focus that redeclare an ancestor's, by member id.
    pub overrides: HashMap<String, OverrideMatch>,
}

fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

fn sort_level(level: &mut [HierarchyEntry]) {
    level.sort_by(|a, b| {
        let rel = |r: HierarchyRelation| {
            if r == HierarchyRelation::Extends {
                0
            } else {
                1
            }
        };
        rel(a.relation)
            .cmp(&rel(b.relation))
            .then_with(|| locale_cmp(&a.node.name, &b.node.name))
            .then_with(|| locale_cmp(&a.node.file_path, &b.node.file_path))
            .then_with(|| a.node.start_line.cmp(&b.node.start_line))
    });
}

fn hierarchy_edges(store: &Store, ids: &[String], up: bool) -> Vec<Edge> {
    let kinds = [EdgeKind::Extends, EdgeKind::Implements];
    let edges = if up {
        store.outgoing_edges_from(ids, &kinds)
    } else {
        store.incoming_edges_to(ids, &kinds)
    };
    edges.unwrap_or_default()
}

fn to_entry(node: Node, depth: usize, parent_id: &str, edge: Edge) -> HierarchyEntry {
    HierarchyEntry {
        node,
        depth,
        parent_id: parent_id.to_string(),
        relation: HierarchyRelation::of(&edge),
        synthesized: edge.provenance.as_deref() == Some("heuristic"),
        edge,
        hidden_subtypes: 0,
    }
}

fn walk_ancestors(store: &Store, focus: &Node) -> Vec<HierarchyEntry> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([focus.id.clone()]);
    let mut frontier = vec![focus.id.clone()];
    for depth in 1..=MAX_ANCESTOR_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let edges = hierarchy_edges(store, &frontier, true);
        if edges.is_empty() {
            break;
        }
        let targets: Vec<String> = edges.iter().map(|e| e.target.clone()).collect();
        let nodes = store.nodes_by_ids(&targets).unwrap_or_default();
        let mut level = Vec::new();
        for edge in edges {
            let Some(node) = nodes.get(&edge.target) else {
                continue;
            };
            if !seen.insert(node.id.clone()) {
                continue;
            }
            let source = edge.source.clone();
            level.push(to_entry(node.clone(), depth, &source, edge));
        }
        sort_level(&mut level);
        frontier = level.iter().map(|e| e.node.id.clone()).collect();
        out.extend(level);
    }
    out
}

struct Down {
    entries: Vec<HierarchyEntry>,
    direct_total: usize,
    direct_implementers: usize,
    bounded: bool,
}

fn walk_descendants(store: &Store, focus: &Node) -> Down {
    let mut entries: Vec<HierarchyEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::from([focus.id.clone()]);
    let mut frontier = vec![focus.id.clone()];
    let mut direct_total = 0;
    let mut direct_implementers = 0;
    let mut bounded = false;

    for depth in 1..=MAX_DESCENDANT_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let edges = hierarchy_edges(store, &frontier, false);
        if edges.is_empty() {
            break;
        }
        let sources: Vec<String> = edges.iter().map(|e| e.source.clone()).collect();
        let nodes = store.nodes_by_ids(&sources).unwrap_or_default();
        let mut level: Vec<HierarchyEntry> = Vec::new();
        let mut overflow: Vec<(String, usize)> = Vec::new();
        let mut level_seen: HashSet<String> = HashSet::new();
        for edge in edges {
            let Some(node) = nodes.get(&edge.source) else {
                continue;
            };
            if seen.contains(&node.id) {
                continue;
            }
            if level_seen.contains(&node.id) {
                // A second edge to an already-seen subtype: `extends` outranks
                // `implements` (a class that both extends and implements).
                if let Some(existing) = level.iter_mut().find(|e| e.node.id == node.id)
                    && existing.relation == HierarchyRelation::Implements
                    && edge.kind == EdgeKind::Extends
                {
                    existing.relation = HierarchyRelation::Extends;
                    existing.synthesized = edge.provenance.as_deref() == Some("heuristic");
                    existing.edge = edge;
                }
                continue;
            }
            if depth == 1 {
                direct_total += 1;
                if edge.kind == EdgeKind::Implements {
                    direct_implementers += 1;
                }
            }
            if entries.len() + level.len() >= MAX_DESCENDANTS {
                bounded = true;
                match overflow.iter_mut().find(|(t, _)| *t == edge.target) {
                    Some((_, n)) => *n += 1,
                    None => overflow.push((edge.target.clone(), 1)),
                }
                level_seen.insert(node.id.clone());
                continue;
            }
            level_seen.insert(node.id.clone());
            let target = edge.target.clone();
            level.push(to_entry(node.clone(), depth, &target, edge));
        }
        for entry in &level {
            seen.insert(entry.node.id.clone());
        }
        sort_level(&mut level);
        for entry in level {
            index.insert(entry.node.id.clone(), entries.len());
            entries.push(entry);
        }
        for (parent_id, count) in overflow {
            if let Some(&at) = index.get(&parent_id) {
                entries[at].hidden_subtypes += count;
            }
        }
        if bounded {
            break;
        }
        frontier = entries
            .iter()
            .filter(|e| e.depth == depth)
            .map(|e| e.node.id.clone())
            .collect();
        if depth == MAX_DESCENDANT_DEPTH && !frontier.is_empty() {
            for edge in hierarchy_edges(store, &frontier, false) {
                if seen.contains(&edge.source) {
                    continue;
                }
                bounded = true;
                if let Some(&at) = index.get(&edge.target) {
                    entries[at].hidden_subtypes += 1;
                }
            }
        }
    }
    Down {
        entries,
        direct_total,
        direct_implementers,
        bounded,
    }
}

fn members_of(store: &Store, container_ids: &[String]) -> Vec<(Node, String)> {
    if container_ids.is_empty() {
        return Vec::new();
    }
    let Ok(edges) = store.outgoing_edges_from(container_ids, &[EdgeKind::Contains]) else {
        return Vec::new();
    };
    if edges.is_empty() {
        return Vec::new();
    }
    let targets: Vec<String> = edges.iter().map(|e| e.target.clone()).collect();
    let nodes = store.nodes_by_ids(&targets).unwrap_or_default();
    let rank: HashMap<&str, usize> = container_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let mut out: Vec<(Node, String)> = edges
        .iter()
        .filter_map(|e| nodes.get(&e.target).map(|n| (n.clone(), e.source.clone())))
        .collect();
    out.sort_by(|a, b| {
        rank.get(a.1.as_str())
            .unwrap_or(&0)
            .cmp(rank.get(b.1.as_str()).unwrap_or(&0))
            .then_with(|| a.0.start_line.cmp(&b.0.start_line))
    });
    out
}

fn match_overrides(
    store: &Store,
    focus: &Node,
    ancestors: &[HierarchyEntry],
) -> HashMap<String, OverrideMatch> {
    let mut result = HashMap::new();
    if ancestors.is_empty() {
        return result;
    }
    let own = members_of(store, std::slice::from_ref(&focus.id));
    if own.is_empty() {
        return result;
    }
    let chain: Vec<&HierarchyEntry> = ancestors.iter().take(MAX_OVERRIDE_ANCESTORS).collect();
    let chain_ids: Vec<String> = chain.iter().map(|a| a.node.id.clone()).collect();
    let base = members_of(store, &chain_ids);
    if base.is_empty() {
        return result;
    }
    let by_ancestor: HashMap<&str, &HierarchyEntry> =
        chain.iter().map(|a| (a.node.id.as_str(), *a)).collect();
    // The nearest declaration of each name wins (members are ranked by chain order).
    let mut by_name: HashMap<&str, (&Node, &str)> = HashMap::new();
    for (member, owner) in &base {
        by_name
            .entry(member.name.as_str())
            .or_insert((member, owner.as_str()));
    }
    for (member, _) in &own {
        if !is_overridable(member.kind) {
            continue;
        }
        let Some((base_member, owner_id)) = by_name.get(member.name.as_str()) else {
            continue;
        };
        if base_member.id == member.id {
            continue;
        }
        let Some(owner) = by_ancestor.get(owner_id) else {
            continue;
        };
        result.insert(
            member.id.clone(),
            OverrideMatch {
                member_id: member.id.clone(),
                base_id: base_member.id.clone(),
                base_type_id: owner.node.id.clone(),
                base_type_name: owner.node.name.clone(),
                relation: owner.relation,
            },
        );
    }
    result
}

/// The hierarchy of `focus`, or `None` for a node that cannot have one or has
/// none at all.
pub fn build_type_hierarchy(store: &Store, focus: &Node) -> Option<TypeHierarchy> {
    if !can_have_hierarchy(focus) {
        return None;
    }
    let ancestors = walk_ancestors(store, focus);
    let down = walk_descendants(store, focus);
    if ancestors.is_empty() && down.entries.is_empty() {
        return None;
    }
    let overrides = match_overrides(store, focus, &ancestors);
    Some(TypeHierarchy {
        focus: focus.clone(),
        polymorphic: down.direct_implementers >= DISPATCH_MIN_IMPLEMENTERS,
        ancestors,
        descendants: down.entries,
        direct_subtypes: down.direct_total,
        direct_implementers: down.direct_implementers,
        bounded: down.bounded,
        overrides,
    })
}
