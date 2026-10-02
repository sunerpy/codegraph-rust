//! `GET /api/node/<id>` — everything the Symbol view draws, in one round-trip.
//! Upstream `src/ui-server/api/node.ts` (+ `hierarchy.ts`).
//!
//! No N+1 anywhere: every edge list is resolved with one batched node lookup and
//! fan-in for the rail pills comes from one batched count. Capped lists still
//! tell the truth: every list carries its true total, and the ordering puts the
//! useful end first (same file, then production code, then tests).
//!
//! Branch conditions on call sites (`when`, family F11) are phase 2: the field
//! is never set here yet.

use std::collections::{BTreeMap, HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_graph::graph::GraphTraverser;
use codegraph_graph::hierarchy::{
    HierarchyEntry, OverrideMatch, build_type_hierarchy, can_have_hierarchy,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::source::{find_indexed_file, has_drifted_on_disk, max_file_size};
use super::wire::*;
use crate::respond::{ApiError, ApiResult};

/// Ancestors kept in the hierarchy block.
pub const MAX_HIERARCHY_ANCESTORS: usize = 24;
/// Descendants kept in the hierarchy block.
pub const MAX_HIERARCHY_DESCENDANTS: usize = 240;

/// A member row in the focal symbol's outline.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMember {
    #[serde(flatten)]
    pub node: WireNodeRef,
    pub parent_id: String,
    pub depth: usize,
    pub fan_in: i64,
    pub fan_out: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Value>,
}

fn override_wire(m: &OverrideMatch) -> Value {
    json!({
        "baseId": m.base_id,
        "baseTypeId": m.base_type_id,
        "baseTypeName": m.base_type_name,
        "relation": m.relation.as_str(),
    })
}

fn hierarchy_node(entry: &HierarchyEntry) -> Value {
    let mut wire = serde_json::to_value(to_node_ref(&entry.node)).unwrap_or(Value::Null);
    wire["depth"] = json!(entry.depth);
    wire["parentId"] = json!(entry.parent_id);
    wire["relation"] = json!(entry.relation.as_str());
    wire["synthesized"] = json!(entry.synthesized);
    wire["hiddenSubtypes"] = json!(entry.hidden_subtypes);
    let meta = entry.edge.metadata.as_ref();
    let via = meta
        .and_then(|m| m.get("synthesizedBy"))
        .and_then(Value::as_str)
        .or_else(|| meta.and_then(|m| m.get("via")).and_then(Value::as_str));
    if let Some(via) = via {
        wire["via"] = json!(via);
    }
    if let Some(at) = meta
        .and_then(|m| m.get("registeredAt"))
        .and_then(Value::as_str)
    {
        wire["registeredAt"] = json!(at);
    }
    wire
}

/// The type hierarchy block and the override marks it puts on the outline.
pub fn build_hierarchy(
    ctx: &Ctx<'_>,
    node: &Node,
) -> Option<(Value, HashMap<String, OverrideMatch>)> {
    if !can_have_hierarchy(node) {
        return None;
    }
    let hierarchy = build_type_hierarchy(ctx.store, node)?;
    let ancestors: Vec<Value> = hierarchy
        .ancestors
        .iter()
        .take(MAX_HIERARCHY_ANCESTORS)
        .map(hierarchy_node)
        .collect();
    let descendants: Vec<Value> = hierarchy
        .descendants
        .iter()
        .take(MAX_HIERARCHY_DESCENDANTS)
        .map(hierarchy_node)
        .collect();
    let wire = json!({
        "ancestors": wire_list(ancestors, hierarchy.ancestors.len()),
        "descendants": wire_list(descendants, hierarchy.descendants.len()),
        "direct": hierarchy.direct_subtypes,
        "implementers": hierarchy.direct_implementers,
        "bounded": hierarchy.bounded,
        "polymorphic": hierarchy.polymorphic,
    });
    Some((wire, hierarchy.overrides))
}

pub fn build(ctx: &Ctx<'_>, node_id: &str) -> ApiResult<Value> {
    let store = ctx.store;
    let Some(node) = store.node_by_id(node_id)? else {
        return Err(ApiError::not_found(
            "No symbol with that id is in this index.",
            Some(
                "Symbol ids change whenever the file is re-indexed — search for the symbol by \
                 name instead of reusing an id from an older session.",
            ),
        ));
    };
    let incoming_all = store.edges_by_target_kind(node_id, None)?;
    let outgoing_all = store.edges_by_source_kind(node_id, None)?;
    let incoming: Vec<&Edge> = incoming_all
        .iter()
        .filter(|e| e.kind != EdgeKind::Contains)
        .collect();
    let mut outgoing_rest: Vec<&Edge> = Vec::new();
    let mut contains_out: Vec<&Edge> = Vec::new();
    for edge in &outgoing_all {
        if edge.kind == EdgeKind::Contains {
            contains_out.push(edge);
        } else {
            outgoing_rest.push(edge);
        }
    }
    let traverser = GraphTraverser::new(store);
    let ancestors = traverser.get_ancestors(node_id)?;

    let mut endpoint_ids: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for id in incoming
        .iter()
        .map(|e| e.source.clone())
        .chain(outgoing_rest.iter().map(|e| e.target.clone()))
        .chain(contains_out.iter().map(|e| e.target.clone()))
    {
        if seen_ids.insert(id.clone()) {
            endpoint_ids.push(id);
        }
    }
    let endpoints = store.nodes_by_ids(&endpoint_ids)?;

    // A `references` edge into a type is "uses type X", not "calls X".
    let mut callee_edges: Vec<&Edge> = Vec::new();
    let mut type_refs: Vec<&Edge> = Vec::new();
    for edge in &outgoing_rest {
        let is_type = endpoints
            .get(&edge.target)
            .is_some_and(|t| is_type_kind(t.kind));
        if edge.kind == EdgeKind::References && is_type {
            type_refs.push(edge);
        } else {
            callee_edges.push(edge);
        }
    }

    let focal_file = to_posix_path(&node.file_path);
    let mut incoming_groups = group_relations(&incoming, |e| e.source.clone(), &endpoints);
    incoming_groups.sort_by(|a, b| {
        let same = |r: &WireRelation| if r.node.file == focal_file { 0 } else { 1 };
        same(a)
            .cmp(&same(b))
            .then_with(|| a.node.test.cmp(&b.node.test))
            .then_with(|| locale_compare(&a.node.file, &b.node.file))
            .then_with(|| first_line(a).cmp(&first_line(b)))
    });
    let mut outgoing_groups = group_relations(&callee_edges, |e| e.target.clone(), &endpoints);
    outgoing_groups.sort_by(|a, b| {
        first_line(a)
            .cmp(&first_line(b))
            .then_with(|| locale_compare(&a.node.name, &b.node.name))
    });
    let mut type_groups = group_relations(&type_refs, |e| e.target.clone(), &endpoints);
    type_groups.sort_by(|a, b| {
        first_line(a)
            .cmp(&first_line(b))
            .then_with(|| locale_compare(&a.node.name, &b.node.name))
    });
    let incoming_total = incoming_groups.len();
    let outgoing_total = outgoing_groups.len();
    let mut shown_incoming: Vec<WireRelation> = incoming_groups
        .iter()
        .take(MAX_INCOMING_GROUPS)
        .cloned()
        .collect();
    let mut shown_outgoing: Vec<WireRelation> = outgoing_groups
        .iter()
        .take(MAX_OUTGOING_GROUPS)
        .cloned()
        .collect();

    let fan_ids: Vec<String> = shown_incoming
        .iter()
        .chain(&shown_outgoing)
        .chain(&type_groups)
        .map(|r| r.node.id.clone())
        .collect();
    let fan_in_of = store.count_incoming_edges(&fan_ids)?;
    for relation in shown_incoming
        .iter_mut()
        .chain(shown_outgoing.iter_mut())
        .chain(type_groups.iter_mut())
    {
        let count = fan_in_of.get(&relation.node.id).copied().unwrap_or(0);
        relation.fan_in = Some(count);
        relation.hub = Some(count >= HUB_THRESHOLD);
    }

    let hierarchy = build_hierarchy(ctx, &node);
    let overrides = hierarchy.as_ref().map(|(_, o)| o);
    let (members, members_total) = build_members(ctx, &node, &contains_out, &endpoints, overrides)?;

    let mut direct_callers: Vec<Node> = Vec::new();
    let mut seen_caller: HashSet<&str> = HashSet::new();
    for edge in &incoming {
        if !is_caller_edge_kind(edge.kind) || !seen_caller.insert(edge.source.as_str()) {
            continue;
        }
        if let Some(source) = endpoints.get(&edge.source) {
            direct_callers.push(source.clone());
        }
    }

    let drift = match find_indexed_file(ctx, &node.file_path)? {
        Some((record, stored)) => has_drifted_on_disk(ctx, &stored, &record, max_file_size(ctx)),
        None => false,
    };

    let mut ancestor_refs: Vec<WireNodeRef> = ancestors.iter().map(to_node_ref).collect();
    ancestor_refs.reverse();
    let types_used_count = type_groups.len();

    Ok(json!({
        "node": to_node_detail(&node),
        "ancestors": ancestor_refs,
        "members": wire_list(members, members_total),
        "hierarchy": hierarchy.as_ref().map(|(w, _)| w.clone()).unwrap_or(Value::Null),
        "incoming": wire_list(shown_incoming, incoming_total),
        "outgoing": wire_list(shown_outgoing, outgoing_total),
        "typesUsed": type_groups,
        "counts": {
            "callers": incoming_total,
            "callees": outgoing_total,
            "typesUsed": types_used_count,
            "fanIn": incoming.len(),
            "fanOut": outgoing_rest.len(),
            "members": members_total,
            "hub": incoming_total as i64 >= HUB_THRESHOLD,
        },
        "tests": summarize_test_callers(&traverser, &direct_callers),
        "outsideIndex": summarize_outside_index(ctx, node_id),
        "blast": summarize_blast(&traverser, &node, incoming_total),
        "drift": drift,
    }))
}

/// The focal symbol's members in source order, one level of nesting deep.
fn build_members(
    ctx: &Ctx<'_>,
    focal: &Node,
    contains_out: &[&Edge],
    endpoints: &HashMap<String, Node>,
    overrides: Option<&HashMap<String, OverrideMatch>>,
) -> ApiResult<(Vec<WireMember>, usize)> {
    let mut direct: Vec<(Node, String, usize)> = Vec::new();
    for edge in contains_out {
        if let Some(child) = endpoints.get(&edge.target) {
            direct.push((child.clone(), focal.id.clone(), 1));
        }
    }
    let container_ids: Vec<String> = direct
        .iter()
        .filter(|(n, _, _)| is_container_kind(n.kind))
        .map(|(n, _, _)| n.id.clone())
        .collect();
    let mut nested: Vec<(Node, String, usize)> = Vec::new();
    if !container_ids.is_empty() {
        let grand_edges = ctx
            .store
            .outgoing_edges_from(&container_ids, &[EdgeKind::Contains])?;
        let targets: Vec<String> = grand_edges.iter().map(|e| e.target.clone()).collect();
        let grand_nodes = ctx.store.nodes_by_ids(&targets)?;
        for edge in &grand_edges {
            if let Some(child) = grand_nodes.get(&edge.target) {
                nested.push((child.clone(), edge.source.clone(), 2));
            }
        }
    }
    let mut all: Vec<(Node, String, usize)> = direct.into_iter().chain(nested).collect();
    all.sort_by(|a, b| {
        a.0.start_line
            .cmp(&b.0.start_line)
            .then_with(|| locale_compare(&a.0.name, &b.0.name))
    });
    let total = all.len();
    let shown: Vec<&(Node, String, usize)> = all.iter().take(MAX_OUTLINE_NODES).collect();
    let ids: Vec<String> = shown.iter().map(|(n, _, _)| n.id.clone()).collect();
    let fan_in = ctx.store.count_incoming_edges(&ids)?;
    let fan_out = ctx.store.count_outgoing_edges(&ids)?;
    let items = shown
        .into_iter()
        .map(|(n, parent, depth)| WireMember {
            node: to_node_ref(n),
            parent_id: parent.clone(),
            depth: *depth,
            fan_in: fan_in.get(&n.id).copied().unwrap_or(0),
            fan_out: fan_out.get(&n.id).copied().unwrap_or(0),
            overrides: overrides.and_then(|o| o.get(&n.id)).map(override_wire),
        })
        .collect();
    Ok((items, total))
}

/// Which tests reach this symbol, the method behind explore's "tests:" line:
/// direct test callers first, then up to two more caller hops, with a budget
/// whose exhaustion is reported rather than papered over.
fn summarize_test_callers(traverser: &GraphTraverser<'_>, direct_callers: &[Node]) -> Value {
    let mut direct_files: Vec<String> = Vec::new();
    for caller in direct_callers {
        let file = to_posix_path(&caller.file_path);
        if is_test_file(&file) && !direct_files.contains(&file) {
            direct_files.push(file);
        }
    }
    if !direct_files.is_empty() {
        return json!({
            "reached": true,
            "hops": 1,
            "fileCount": direct_files.len(),
            "files": direct_files.iter().take(MAX_TEST_FILES).collect::<Vec<_>>(),
            "exhaustive": true,
            "hopsSearched": 1,
        });
    }
    let mut budget = TEST_CALLER_BUDGET as i64;
    let mut visited: HashSet<String> = direct_callers.iter().map(|n| n.id.clone()).collect();
    let mut frontier: Vec<Node> = direct_callers.to_vec();
    let mut hops_searched = 1;
    let mut hop = 2;
    while hop <= TEST_CALLER_HOPS && !frontier.is_empty() && budget > 0 {
        hops_searched = hop;
        let mut next: Vec<Node> = Vec::new();
        let mut found: Vec<String> = Vec::new();
        for current in &frontier {
            let spend = budget;
            budget -= 1;
            if spend <= 0 {
                break;
            }
            let Ok(callers) = traverser.get_callers(&current.id, 1) else {
                continue;
            };
            for caller in callers {
                let source = caller.node;
                if !visited.insert(source.id.clone()) {
                    continue;
                }
                let file = to_posix_path(&source.file_path);
                if is_test_file(&file) {
                    if !found.contains(&file) {
                        found.push(file);
                    }
                } else {
                    next.push(source);
                }
            }
        }
        if !found.is_empty() {
            return json!({
                "reached": true,
                "hops": hop,
                "fileCount": found.len(),
                "files": found.iter().take(MAX_TEST_FILES).collect::<Vec<_>>(),
                "exhaustive": true,
                "hopsSearched": hop,
            });
        }
        frontier = next;
        hop += 1;
    }
    json!({
        "reached": false,
        "hops": Value::Null,
        "fileCount": 0,
        "files": [],
        "exhaustive": budget > 0,
        "hopsSearched": hops_searched,
    })
}

/// Calls and type mentions from this symbol that never resolved to a node.
fn summarize_outside_index(ctx: &Ctx<'_>, node_id: &str) -> Value {
    let Ok(refs) = ctx.store.unresolved_refs_from(node_id) else {
        return json!({ "total": 0, "byKind": {}, "samples": [] });
    };
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &refs {
        *by_kind.entry(r.reference_kind.as_str()).or_insert(0) += 1;
    }
    let mut sorted: Vec<_> = refs.iter().collect();
    sorted.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.col.cmp(&b.col)));
    let samples: Vec<Value> = sorted
        .iter()
        .take(MAX_OUTSIDE_INDEX_SAMPLES)
        .map(|r| {
            json!({
                "name": r.reference_name,
                "kind": r.reference_kind.as_str(),
                "line": r.line,
                "col": r.col,
            })
        })
        .collect();
    json!({ "total": refs.len(), "byKind": by_kind, "samples": samples })
}

/// What would need re-checking if this symbol changed: the depth-3 impact radius.
fn summarize_blast(traverser: &GraphTraverser<'_>, node: &Node, direct: usize) -> Value {
    let Ok(subgraph) = traverser.get_impact_radius(&node.id, BLAST_DEPTH) else {
        return Value::Null;
    };
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut routes = 0;
    for (id, dependent) in &subgraph.nodes {
        if *id == node.id {
            continue;
        }
        *per_file
            .entry(to_posix_path(&dependent.file_path))
            .or_insert(0) += 1;
        if dependent.kind == NodeKind::Route {
            routes += 1;
        }
    }
    let test_files = per_file.keys().filter(|f| is_test_file(f)).count();
    let mut top: Vec<(&String, &usize)> = per_file.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| locale_compare(a.0, b.0)));
    let top_files: Vec<Value> = top
        .into_iter()
        .take(40)
        .map(|(file, symbols)| json!({ "file": file, "symbols": symbols, "test": is_test_file(file) }))
        .collect();
    json!({
        "direct": direct,
        "withinHops": subgraph.nodes.len().saturating_sub(1),
        "hops": BLAST_DEPTH,
        "files": per_file.len(),
        "testFiles": test_files,
        "routes": routes,
        "topFiles": top_files,
    })
}
