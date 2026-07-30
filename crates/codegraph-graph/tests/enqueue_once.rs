//! Upstream #1359 — `find_path`'s BFS must enqueue each work item EXACTLY once.
//!
//! `traverse_bfs` got a separate `enqueued` guard in #1090; `find_path` did not.
//! Its `visited` set is populated at DEQUEUE, so on a fan-in layer (k nodes all
//! pointing into the same k targets — a shared-helper hub, common in real call
//! graphs) the same target is pushed once per predecessor, up to k² pushes for
//! that transition, each carrying a cloned path array.
//!
//! The shortest path itself stays correct, so the defect is invisible from
//! `find_path`'s return value. These tests assert on
//! `find_path_instrumented`'s queue accounting instead, which is why the
//! instrumentation is part of the fix rather than a log line.

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_graph::graph::GraphTraverser;
use codegraph_store::Store;

fn temp_db_path(label: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "codegraph-enqueue-once-{label}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

fn node(id: &str, name: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: "src/hub.ts".to_string(),
        language: Language::TypeScript,
        start_line: 1,
        end_line: 2,
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
        line: Some(3),
        col: Some(0),
        provenance: None,
    }
}

/// A fan-in graph: `root` → `mid0..mid{k-1}`, and EVERY mid → EVERY
/// `sink0..sink{k-1}`. `target` hangs off `sink0` so the search must traverse
/// the whole fan-in layer before finding it.
fn fan_in_store(label: &str, k: usize) -> (Store, usize) {
    let mut store = Store::open(&temp_db_path(label)).expect("open store");
    let mut nodes = vec![
        node("function:root", "root"),
        node("function:target", "target"),
    ];
    for i in 0..k {
        nodes.push(node(&format!("function:mid{i}"), &format!("mid{i}")));
        nodes.push(node(&format!("function:sink{i}"), &format!("sink{i}")));
    }
    let reachable = nodes.len();
    store.upsert_nodes(&nodes).expect("insert nodes");

    let mut edges = Vec::new();
    for i in 0..k {
        edges.push(edge("function:root", &format!("function:mid{i}")));
        for j in 0..k {
            edges.push(edge(
                &format!("function:mid{i}"),
                &format!("function:sink{j}"),
            ));
        }
    }
    edges.push(edge("function:sink0", "function:target"));
    store.insert_edges(&edges).expect("insert edges");
    (store, reachable)
}

/// The enqueue-once contract: over a fan-in hub, the number of pushes must be
/// bounded by the number of distinct nodes reached — never by the edge count.
#[test]
fn find_path_enqueues_each_work_item_exactly_once_over_a_fan_in_hub() {
    let k = 8;
    let (store, reachable) = fan_in_store("fanin", k);
    let traverser = GraphTraverser::new(&store);

    let (path, stats) = traverser
        .find_path_instrumented("function:root", "function:target", &[EdgeKind::Calls])
        .expect("find_path_instrumented");

    assert!(path.is_some(), "the fan-in graph does contain a path");
    assert!(
        stats.enqueued <= reachable,
        "each work item must be enqueued at most once: enqueued {} for {reachable} reachable nodes (fan-in edges: {}) — stats {stats:?}",
        stats.enqueued,
        k * k + k + 1
    );
    assert_eq!(
        stats.duplicate_dequeues, 0,
        "a correctly guarded queue never dequeues an already-visited item: {stats:?}"
    );
}

/// The same contract on a diamond — the smallest shape that double-enqueues.
#[test]
fn find_path_does_not_re_enqueue_a_shared_successor_in_a_diamond() {
    let mut store = Store::open(&temp_db_path("diamond")).expect("open store");
    store
        .upsert_nodes(&[
            node("function:a", "a"),
            node("function:b", "b"),
            node("function:c", "c"),
            node("function:d", "d"),
            node("function:e", "e"),
        ])
        .expect("insert nodes");
    store
        .insert_edges(&[
            edge("function:a", "function:b"),
            edge("function:a", "function:c"),
            edge("function:b", "function:d"),
            edge("function:c", "function:d"),
            edge("function:d", "function:e"),
        ])
        .expect("insert edges");

    let traverser = GraphTraverser::new(&store);
    let (path, stats) = traverser
        .find_path_instrumented("function:a", "function:e", &[EdgeKind::Calls])
        .expect("find_path_instrumented");

    assert!(path.is_some(), "a→b→d→e exists");
    assert_eq!(
        stats.enqueued, 5,
        "a, b, c, d, e — `d` is reachable from both b and c but must be enqueued ONCE: {stats:?}"
    );
    assert_eq!(stats.duplicate_dequeues, 0, "{stats:?}");
}

/// The guard must not change the ANSWER: the shortest path is still the shortest
/// path, and an unreachable target is still unreachable.
#[test]
fn enqueue_once_guard_preserves_the_shortest_path_and_the_no_path_answer() {
    let mut store = Store::open(&temp_db_path("shortest")).expect("open store");
    store
        .upsert_nodes(&[
            node("function:a", "a"),
            node("function:short", "short"),
            node("function:long1", "long1"),
            node("function:long2", "long2"),
            node("function:z", "z"),
            node("function:island", "island"),
        ])
        .expect("insert nodes");
    store
        .insert_edges(&[
            edge("function:a", "function:long1"),
            edge("function:long1", "function:long2"),
            edge("function:long2", "function:z"),
            edge("function:a", "function:short"),
            edge("function:short", "function:z"),
        ])
        .expect("insert edges");

    let traverser = GraphTraverser::new(&store);
    let path = traverser
        .find_path("function:a", "function:z", &[EdgeKind::Calls])
        .expect("find_path")
        .expect("a path exists");
    let names: Vec<&str> = path.iter().map(|s| s.node.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a", "short", "z"],
        "the shortest of two routes must still win"
    );

    assert!(
        traverser
            .find_path("function:a", "function:island", &[EdgeKind::Calls])
            .expect("find_path")
            .is_none(),
        "an unreachable target must still report no path"
    );
}

/// A cycle must still terminate with the guard in place.
#[test]
fn enqueue_once_guard_still_terminates_on_a_cycle() {
    let mut store = Store::open(&temp_db_path("cycle")).expect("open store");
    store
        .upsert_nodes(&[
            node("function:a", "a"),
            node("function:b", "b"),
            node("function:c", "c"),
            node("function:unreached", "unreached"),
        ])
        .expect("insert nodes");
    store
        .insert_edges(&[
            edge("function:a", "function:b"),
            edge("function:b", "function:c"),
            edge("function:c", "function:a"),
        ])
        .expect("insert edges");

    let traverser = GraphTraverser::new(&store);
    let (path, stats) = traverser
        .find_path_instrumented("function:a", "function:unreached", &[EdgeKind::Calls])
        .expect("find_path_instrumented");
    assert!(path.is_none(), "`unreached` has no incoming edge");
    assert_eq!(
        stats.enqueued, 3,
        "a, b, c each enqueued once despite the cycle: {stats:?}"
    );
}
