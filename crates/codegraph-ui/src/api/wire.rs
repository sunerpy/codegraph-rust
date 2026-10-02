//! The wire shapes the viewer reads, and the rules for producing them — upstream
//! `src/ui-server/api/wire.ts`.
//!
//! One round-trip per screen, and capped lists with honest totals: a symbol with
//! 545 callers cannot ship 545 rows, but every capped list carries the true
//! `total` beside the `shown` slice so the UI can say "+N more".

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use serde::Serialize;
use serde_json::Value;

/// Fan-in at or above which a symbol is a "hub".
pub const HUB_THRESHOLD: i64 = 40;
/// Below this resolution confidence an edge is a name-only guess.
pub const UNCERTAIN_BELOW: f64 = 0.6;
/// Caller groups returned for a node.
pub const MAX_INCOMING_GROUPS: usize = 300;
/// Callee groups returned for a node.
pub const MAX_OUTGOING_GROUPS: usize = 200;
/// Edges kept inside a single group.
pub const MAX_EDGES_PER_GROUP: usize = 40;
/// Test files named in a node's test-caller summary.
pub const MAX_TEST_FILES: usize = 6;
/// Dependency hops the blast-radius summary walks.
pub const BLAST_DEPTH: usize = 3;
/// Caller hops walked looking for a test.
pub const TEST_CALLER_HOPS: usize = 3;
/// `getCallers` lookups the test walk may spend.
pub const TEST_CALLER_BUDGET: usize = 64;
/// Unresolved references listed by name before the payload just counts them.
pub const MAX_OUTSIDE_INDEX_SAMPLES: usize = 40;
/// Symbols in a file outline.
pub const MAX_OUTLINE_NODES: usize = 3000;
/// Files listed in each direction of the File view's import rails.
pub const MAX_IMPORT_FILES: usize = 300;

/// Project-relative, forward slashes on every platform.
pub fn to_posix_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// "Test" in the wide reading — tests, examples, fixtures, benchmarks, demos —
/// as upstream's `isTestFile`.
pub fn is_test_file(path: &str) -> bool {
    codegraph_graph::query::scoring::is_test_file(path)
}

/// "Test" in the narrow reading — a suite that exercises other code, and not
/// an example or a fixture — as upstream's `isTestPath`.
pub fn is_test_path(path: &str) -> bool {
    codegraph_graph::query::scoring::is_test_path(path)
}

/// A symbol as it appears in a rail, an outline or a search result.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireNodeRef {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub line: i64,
    pub end_line: i64,
    pub language: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exported: Option<bool>,
    pub test: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<bool>,
}

pub fn to_node_ref(node: &Node) -> WireNodeRef {
    let file = to_posix_path(&node.file_path);
    WireNodeRef {
        id: node.id.clone(),
        kind: node.kind.as_str(),
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        test: is_test_file(&file),
        file,
        line: node.start_line,
        end_line: node.end_line,
        language: node.language.as_str(),
        signature: node.signature.clone().filter(|s| !s.is_empty()),
        exported: node.is_exported.then_some(true),
        generated: None,
    }
}

/// The focal symbol of a Symbol view: the ref plus everything the header shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireNodeDetail {
    #[serde(flatten)]
    pub node: WireNodeRef,
    pub start_column: i64,
    pub end_column: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(rename = "async", skip_serializing_if = "Option::is_none")]
    pub is_async: Option<bool>,
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub is_static: Option<bool>,
    #[serde(rename = "abstract", skip_serializing_if = "Option::is_none")]
    pub is_abstract: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decorators: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_parameters: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    pub lines: i64,
}

pub fn to_node_detail(node: &Node) -> WireNodeDetail {
    WireNodeDetail {
        node: to_node_ref(node),
        start_column: node.start_column,
        end_column: node.end_column,
        docstring: node.docstring.clone().filter(|s| !s.is_empty()),
        visibility: node.visibility.clone().filter(|s| !s.is_empty()),
        is_async: node.is_async.then_some(true),
        is_static: node.is_static.then_some(true),
        is_abstract: node.is_abstract.then_some(true),
        decorators: (!node.decorators.is_empty()).then(|| node.decorators.clone()),
        type_parameters: (!node.type_parameters.is_empty()).then(|| node.type_parameters.clone()),
        return_type: node.return_type.clone().filter(|s| !s.is_empty()),
        lines: (node.end_line - node.start_line + 1).max(1),
    }
}

/// One edge, flattened to the fields the viewer draws with.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireEdge {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesized_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_ref: Option<bool>,
    /// The conditions the call site runs under (family F11, phase 2). Never set
    /// by this server yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

fn meta_str(meta: Option<&Value>, key: &str) -> Option<String> {
    meta.and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The edge's recorded confidence, when it carries one.
pub fn edge_confidence(edge: &Edge) -> Option<f64> {
    edge.metadata
        .as_ref()
        .and_then(|m| m.get("confidence"))
        .and_then(Value::as_f64)
}

pub fn to_wire_edge(edge: &Edge) -> WireEdge {
    let meta = edge.metadata.as_ref();
    WireEdge {
        kind: edge.kind.as_str(),
        line: edge.line,
        col: edge.col,
        confidence: edge_confidence(edge),
        resolved_by: meta_str(meta, "resolvedBy"),
        provenance: edge.provenance.clone().filter(|p| !p.is_empty()),
        synthesized_by: meta_str(meta, "synthesizedBy"),
        via: meta_str(meta, "via"),
        registered_at: meta_str(meta, "registeredAt"),
        value_ref: meta
            .and_then(|m| m.get("valueRef"))
            .and_then(Value::as_bool)
            .filter(|v| *v),
        when: None,
    }
}

/// Every edge between the focal symbol and ONE other symbol, as a single row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRelation {
    pub node: WireNodeRef,
    pub edge_kinds: Vec<&'static str>,
    pub edges: Vec<WireEdge>,
    pub edge_count: usize,
    pub lines: Vec<i64>,
    pub confidence: Option<f64>,
    pub uncertain: bool,
    pub synthesized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_in: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<bool>,
}

/// A capped list that still knows how long it really is.
#[derive(Debug, Clone, Serialize)]
pub struct WireList<T: Serialize> {
    pub total: usize,
    pub shown: usize,
    pub truncated: bool,
    pub items: Vec<T>,
}

pub fn wire_list<T: Serialize>(items: Vec<T>, total: usize) -> WireList<T> {
    WireList {
        total,
        shown: items.len(),
        truncated: items.len() < total,
        items,
    }
}

/// Fold edges into one relation per counterpart symbol. `other` names which end
/// of each edge is the OTHER symbol; `nodes` is the batch-resolved endpoints.
/// Groups keep first-seen order, as upstream's `Map` does.
pub fn group_relations<F>(
    edges: &[&Edge],
    other: F,
    nodes: &HashMap<String, Node>,
) -> Vec<WireRelation>
where
    F: Fn(&Edge) -> String,
{
    let mut order: Vec<String> = Vec::new();
    let mut by_node: HashMap<String, Vec<&Edge>> = HashMap::new();
    for edge in edges {
        let id = other(edge);
        by_node
            .entry(id.clone())
            .or_insert_with(|| {
                order.push(id.clone());
                Vec::new()
            })
            .push(edge);
    }
    let mut relations = Vec::new();
    for id in order {
        let Some(node) = nodes.get(&id) else {
            continue;
        };
        let mut ordered = by_node.remove(&id).unwrap_or_default();
        // Stable, like `Array.prototype.sort`.
        ordered.sort_by_key(|e| e.line.unwrap_or(0));
        let wire_edges = ordered
            .iter()
            .take(MAX_EDGES_PER_GROUP)
            .map(|e| to_wire_edge(e))
            .collect();
        let mut edge_kinds: Vec<&'static str> = Vec::new();
        for edge in &ordered {
            let kind = edge.kind.as_str();
            if !edge_kinds.contains(&kind) {
                edge_kinds.push(kind);
            }
        }
        let mut lines: Vec<i64> = ordered
            .iter()
            .filter_map(|e| e.line)
            .filter(|l| *l > 0)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        lines.sort_unstable();
        let mut confidence: Option<f64> = None;
        let mut synthesized = false;
        for edge in &ordered {
            if let Some(value) = edge_confidence(edge)
                && confidence.is_none_or(|c| value > c)
            {
                confidence = Some(value);
            }
            if edge.provenance.as_deref() == Some("heuristic") {
                synthesized = true;
            }
        }
        relations.push(WireRelation {
            node: to_node_ref(node),
            edge_kinds,
            edges: wire_edges,
            edge_count: ordered.len(),
            lines,
            confidence,
            uncertain: confidence.is_some_and(|c| c < UNCERTAIN_BELOW),
            synthesized,
            fan_in: None,
            hub: None,
        });
    }
    relations
}

/// First call-site line of a relation; unlined rows sort last.
pub fn first_line(relation: &WireRelation) -> i64 {
    relation.lines.first().copied().unwrap_or(i64::MAX)
}

/// Node kinds that count as "a type" for the "types used" chips.
pub fn is_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Interface
            | NodeKind::TypeAlias
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Union
            | NodeKind::Trait
            | NodeKind::Protocol
    )
}

/// Container kinds whose members the outline nests one level deeper.
pub fn is_container_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File
            | NodeKind::Module
            | NodeKind::Namespace
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Union
    )
}

/// The edge kinds `getCallers` treats as "reaches this symbol" (`navigates` has
/// no Rust edge kind yet; nothing can carry it).
pub fn is_caller_edge_kind(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports | EdgeKind::Instantiates
    )
}

/// `String.prototype.localeCompare` (ICU root collation) for the paths and
/// names the viewer sorts — shared with the engine's own viewer-facing reports.
pub use codegraph_graph::collation::locale_compare;
