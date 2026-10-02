//! Where a flow stops: the calls out of the last symbol a path reached, split
//! into the ones the resolver was sure of and the name-only guesses under the
//! confidence floor that the search refused to follow. A port of upstream
//! `continuationsFrom` (`src/graph/dynamic-boundary-report.ts`, `v1.6.1`).
//!
//! The uncertain half is the honest one: an unfollowed guess left invisible
//! reads as "there is nothing here".

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{EdgeKind, Node};
use codegraph_store::Store;

/// Confidence below which a resolved edge is a name-only guess.
pub const UNCERTAIN_BELOW: f64 = 0.6;

/// A call out of the stopping symbol, and how sure the resolver was.
#[derive(Debug, Clone)]
pub struct BoundaryContinuation {
    pub node: Node,
    pub line: Option<i64>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct BoundaryContinuations {
    pub resolved: Vec<BoundaryContinuation>,
    pub uncertain: Vec<BoundaryContinuation>,
}

/// The kinds that continue a flow (`navigates` has no Rust edge kind yet).
fn is_continuation_kind(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::Calls | EdgeKind::Instantiates)
}

/// One row per target and bucket, in line order; `exclude` drops the symbols
/// already on the path.
pub fn continuations_from(
    store: &Store,
    node: &Node,
    exclude: &HashSet<String>,
) -> rusqlite::Result<BoundaryContinuations> {
    let mut resolved: Vec<BoundaryContinuation> = Vec::new();
    let mut uncertain: Vec<BoundaryContinuation> = Vec::new();
    let mut seen_resolved: HashSet<String> = HashSet::new();
    let mut seen_uncertain: HashSet<String> = HashSet::new();
    let edges = store.edges_by_source_kind(&node.id, None)?;
    let targets: Vec<String> = edges.iter().map(|e| e.target.clone()).collect();
    let nodes: HashMap<String, Node> = store.nodes_by_ids(&targets)?;
    for edge in edges {
        if !is_continuation_kind(edge.kind)
            || edge.target == node.id
            || exclude.contains(&edge.target)
        {
            continue;
        }
        let confidence = edge
            .metadata
            .as_ref()
            .and_then(|m| m.get("confidence"))
            .and_then(serde_json::Value::as_f64);
        let is_uncertain = confidence.is_some_and(|c| c < UNCERTAIN_BELOW);
        let (bucket, seen) = if is_uncertain {
            (&mut uncertain, &mut seen_uncertain)
        } else {
            (&mut resolved, &mut seen_resolved)
        };
        if seen.contains(&edge.target) {
            continue;
        }
        let Some(target) = nodes.get(&edge.target) else {
            continue;
        };
        seen.insert(edge.target.clone());
        bucket.push(BoundaryContinuation {
            node: target.clone(),
            line: edge.line,
            confidence,
        });
    }
    resolved.sort_by_key(|c| c.line.unwrap_or(0));
    uncertain.sort_by_key(|c| c.line.unwrap_or(0));
    Ok(BoundaryContinuations {
        resolved,
        uncertain,
    })
}
