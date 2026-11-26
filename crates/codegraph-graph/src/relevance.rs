//! Graph-connectivity relevance for explore (W7-25b): upstream
//! `computeGraphRelevance` (`src/mcp/tools.ts`, `b635dd46` 4047-4102,
//! `e8ddc4e2` 4131-4196), a random walk with restart from the query's seeds.
//!
//! The walk runs over the explore subgraph only, its edges taken as
//! undirected, so a node call-connected to the seeds accrues mass while a
//! lone text match keeps little more than its restart share. A node with no
//! edge keeps its own mass. A self-loop and an edge to a node outside the
//! subgraph are skipped.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind};

/// The edges the walk follows (upstream `RANK_EDGES`).
const RANK_EDGES: [EdgeKind; 10] = [
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::Overrides,
    EdgeKind::Instantiates,
    EdgeKind::Returns,
    EdgeKind::TypeOf,
    EdgeKind::Imports,
    EdgeKind::Navigates,
];

/// The restart probability.
const ALPHA: f64 = 0.25;

/// The power iterations.
const ITERATIONS: usize = 25;

/// Each of `node_ids`' walk mass, in their order. The restart vector is
/// uniform over the seeds in `node_ids`, or over every node when none is.
///
/// The walk runs over the nodes sorted by id, whatever order the caller holds
/// the subgraph in. Each node's inflow is summed in that order, so neither the
/// node order nor the edge order can change a bit, and a synced index and a
/// fresh one, whose rows come back in different orders, get the same masses.
pub fn graph_relevance(node_ids: &[String], edges: &[Edge], seeds: &HashSet<String>) -> Vec<f64> {
    if node_ids.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<&str> = node_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();
    let n = ids.len();
    let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in edges.iter().filter(|edge| RANK_EDGES.contains(&edge.kind)) {
        let (Some(&i), Some(&j)) = (
            index.get(edge.source.as_str()),
            index.get(edge.target.as_str()),
        ) else {
            continue;
        };
        if i == j {
            continue;
        }
        adjacency[i].push(j);
        adjacency[j].push(i);
    }
    let mut restart = vec![0.0f64; n];
    let mut total = 0.0f64;
    for (i, id) in ids.iter().enumerate() {
        if seeds.contains(*id) {
            restart[i] = 1.0;
            total += 1.0;
        }
    }
    if total == 0.0 {
        restart.iter_mut().for_each(|r| *r = 1.0);
        total = n as f64;
    }
    restart.iter_mut().for_each(|r| *r /= total);
    let mut mass = restart.clone();
    for _ in 0..ITERATIONS {
        let mut next = vec![0.0f64; n];
        for i in 0..n {
            let share_of = mass[i];
            if share_of == 0.0 {
                continue;
            }
            let degree = adjacency[i].len();
            if degree == 0 {
                next[i] += share_of;
                continue;
            }
            let share = share_of / degree as f64;
            for &j in &adjacency[i] {
                next[j] += share;
            }
        }
        for i in 0..n {
            mass[i] = (1.0 - ALPHA) * next[i] + ALPHA * restart[i];
        }
    }
    node_ids.iter().map(|id| mass[index[id.as_str()]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::EdgeKind;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i}")).collect()
    }

    fn edge(source: usize, target: usize, kind: EdgeKind) -> Edge {
        Edge {
            id: None,
            source: format!("n{source}"),
            target: format!("n{target}"),
            kind,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    fn seeds(of: &[usize]) -> HashSet<String> {
        of.iter().map(|i| format!("n{i}")).collect()
    }

    fn close(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-15, "{actual:?} vs {expected:?}");
        }
    }

    /// Two connected nodes, the walk restarting at the first: upstream's
    /// numbers (α 0.25, 25 iterations), computed by hand in the same order.
    #[test]
    fn a_pair_shares_the_walk_by_the_restart() {
        let mass = graph_relevance(&ids(2), &[edge(0, 1, EdgeKind::Calls)], &seeds(&[0]));
        close(&mass, &[0.5711060528036436, 0.42889394719635643]);
    }

    /// A path of three: the middle node collects from both sides.
    #[test]
    fn a_path_of_three() {
        let mass = graph_relevance(
            &ids(3),
            &[
                edge(0, 1, EdgeKind::Calls),
                edge(1, 2, EdgeKind::References),
            ],
            &seeds(&[0]),
        );
        close(
            &mass,
            &[0.4105530264018218, 0.42889394719635643, 0.16055302640182179],
        );
    }

    /// A node without an edge keeps its own mass; nodes the walk never
    /// reaches have none.
    #[test]
    fn a_node_without_edges_keeps_its_mass() {
        let mass = graph_relevance(&ids(3), &[edge(1, 2, EdgeKind::Calls)], &seeds(&[0]));
        close(&mass, &[1.0, 0.0, 0.0]);
    }

    /// With no seed among the nodes the restart is uniform.
    #[test]
    fn no_seed_restarts_everywhere() {
        let mass = graph_relevance(&ids(2), &[], &seeds(&[7]));
        close(&mass, &[0.5, 0.5]);
        assert!(graph_relevance(&[], &[], &seeds(&[0])).is_empty());
    }

    /// `contains`, a self-loop and an edge to a node outside the set carry no
    /// mass; the rank kinds do.
    #[test]
    fn only_rank_edges_between_known_nodes_count() {
        let mut out_of_set = edge(0, 1, EdgeKind::Calls);
        out_of_set.target = "elsewhere".to_string();
        let ignored = graph_relevance(
            &ids(2),
            &[
                edge(0, 1, EdgeKind::Contains),
                edge(0, 0, EdgeKind::Calls),
                out_of_set,
            ],
            &seeds(&[0]),
        );
        close(&ignored, &[1.0, 0.0]);
        for kind in [
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::Overrides,
            EdgeKind::Instantiates,
            EdgeKind::Returns,
            EdgeKind::TypeOf,
            EdgeKind::Imports,
            EdgeKind::Navigates,
        ] {
            let mass = graph_relevance(&ids(2), &[edge(0, 1, kind)], &seeds(&[0]));
            assert!(mass[1] > 0.0, "{kind:?} carries no mass");
        }
    }

    /// The same nodes and edges in another order give the same bits for each
    /// node: the walk runs in its own canonical order.
    #[test]
    fn input_order_does_not_change_the_bits() {
        let edges = vec![
            edge(0, 1, EdgeKind::Calls),
            edge(1, 2, EdgeKind::Calls),
            edge(2, 3, EdgeKind::References),
            edge(0, 3, EdgeKind::Imports),
            edge(1, 3, EdgeKind::Calls),
            edge(4, 1, EdgeKind::Calls),
            edge(4, 3, EdgeKind::References),
            edge(5, 2, EdgeKind::Calls),
            edge(5, 0, EdgeKind::Navigates),
        ];
        let mut reversed = edges.clone();
        reversed.reverse();
        let mut shuffled = ids(6);
        shuffled.reverse();
        shuffled.swap(1, 4);
        let a = graph_relevance(&ids(6), &edges, &seeds(&[0, 2, 5]));
        let b = graph_relevance(&shuffled, &reversed, &seeds(&[0, 2, 5]));
        let by_id = |order: &[String], mass: &[f64]| {
            let mut pairs: Vec<(String, u64)> = order
                .iter()
                .cloned()
                .zip(mass.iter().map(|m| m.to_bits()))
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(by_id(&ids(6), &a), by_id(&shuffled, &b));
        assert!(a.iter().all(|m| *m > 0.0));
    }

    /// The answer follows the caller's order, a repeated id included.
    #[test]
    fn the_answer_follows_the_callers_order() {
        let mut order = ids(3);
        order.reverse();
        order.push("n2".to_string());
        let mass = graph_relevance(
            &order,
            &[
                edge(0, 1, EdgeKind::Calls),
                edge(1, 2, EdgeKind::References),
            ],
            &seeds(&[0]),
        );
        close(
            &mass,
            &[
                0.16055302640182179,
                0.42889394719635643,
                0.4105530264018218,
                0.16055302640182179,
            ],
        );
    }
}
