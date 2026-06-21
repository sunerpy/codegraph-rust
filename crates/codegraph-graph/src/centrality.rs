//! Deterministic node-centrality scores over the code graph.
//!
//! Pure-Rust PageRank + in/out-degree — no ML/vector crates, so it stays within
//! the `no-AI` scope guardrail and is byte-reproducible (fixed iteration count,
//! id-sorted node order, no float-order nondeterminism). `contains` edges are
//! excluded: they are structural (file→symbol) not dependency signal, mirroring
//! the dependency-graph convention used by cycle detection. The PageRank score
//! is exposed as a node's `god_score` (high-centrality = "god node").

use std::collections::HashMap;

use codegraph_core::types::{Edge, EdgeKind, Node};

/// Per-node centrality. `pagerank` sums to ~1.0 across nodes; `god_score`
/// aliases it (downstream "god node" ranking).
#[derive(Debug, Clone, Copy)]
pub struct Centrality {
    pub pagerank: f64,
    pub in_degree: u32,
    pub out_degree: u32,
}

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 30;

/// Compute centrality for every node id. Deterministic: nodes are processed in
/// the given (id-sorted) order and the power-iteration runs a fixed number of
/// rounds, so the result is identical run-to-run.
pub fn compute(nodes: &[Node], edges: &[Edge]) -> HashMap<String, Centrality> {
    let n = nodes.len();
    let mut scores: HashMap<String, Centrality> = HashMap::with_capacity(n);
    for node in nodes {
        scores.insert(
            node.id.clone(),
            Centrality {
                pagerank: 0.0,
                in_degree: 0,
                out_degree: 0,
            },
        );
    }
    if n == 0 {
        return scores;
    }

    // Adjacency over dependency edges only (exclude `contains`). Endpoints must
    // both be known nodes — orphan edges are skipped (they carry no signal).
    let mut out_adj: HashMap<&str, Vec<&str>> = HashMap::with_capacity(n);
    for edge in edges {
        if edge.kind == EdgeKind::Contains {
            continue;
        }
        if !scores.contains_key(&edge.source) || !scores.contains_key(&edge.target) {
            continue;
        }
        out_adj
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
        if let Some(c) = scores.get_mut(&edge.source) {
            c.out_degree += 1;
        }
        if let Some(c) = scores.get_mut(&edge.target) {
            c.in_degree += 1;
        }
    }

    // Power iteration. `ids` fixes a stable processing order so the dangling-mass
    // redistribution and the final assignment are deterministic.
    let ids: Vec<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let base = 1.0 / n as f64;
    let mut rank: HashMap<&str, f64> = ids.iter().map(|id| (*id, base)).collect();

    for _ in 0..ITERATIONS {
        let mut next: HashMap<&str, f64> = ids.iter().map(|id| (*id, 0.0)).collect();
        let mut dangling = 0.0;
        for id in &ids {
            let r = rank[id];
            match out_adj.get(id) {
                Some(targets) if !targets.is_empty() => {
                    let share = r / targets.len() as f64;
                    for t in targets {
                        *next.get_mut(t).expect("target is a known node") += share;
                    }
                }
                // No out-edges: its rank is dangling mass, spread to everyone.
                _ => dangling += r,
            }
        }
        let teleport = (1.0 - DAMPING) / n as f64 + DAMPING * dangling / n as f64;
        for id in &ids {
            let inflow = next[id];
            rank.insert(id, teleport + DAMPING * inflow);
        }
    }

    for id in &ids {
        if let Some(c) = scores.get_mut(*id) {
            c.pagerank = rank[id];
        }
    }
    scores
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Language, NodeKind};

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "src/lib.rs".to_string(),
            language: Language::Rust,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 1,
        }
    }

    fn edge(source: &str, target: &str) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    #[test]
    fn most_referenced_node_ranks_highest_and_is_deterministic() {
        // a, b, c all call hub; hub calls nothing → hub has highest PageRank.
        let nodes = vec![node("a"), node("b"), node("c"), node("hub")];
        let edges = vec![edge("a", "hub"), edge("b", "hub"), edge("c", "hub")];

        let first = compute(&nodes, &edges);
        let second = compute(&nodes, &edges);

        let hub = first["hub"];
        assert_eq!(hub.in_degree, 3);
        assert_eq!(hub.out_degree, 0);
        assert!(
            hub.pagerank > first["a"].pagerank,
            "hub must outrank its callers"
        );
        // Determinism: identical inputs → byte-identical scores.
        assert_eq!(hub.pagerank, second["hub"].pagerank);
    }

    #[test]
    fn contains_edges_are_excluded_from_centrality() {
        let nodes = vec![node("file"), node("sym")];
        let mut contains = edge("file", "sym");
        contains.kind = EdgeKind::Contains;
        let scores = compute(&nodes, &[contains]);
        // `contains` carries no dependency signal → zero degrees.
        assert_eq!(scores["sym"].in_degree, 0);
        assert_eq!(scores["file"].out_degree, 0);
    }
}
