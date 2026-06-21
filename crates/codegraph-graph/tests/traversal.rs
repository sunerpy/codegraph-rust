use std::collections::BTreeSet;

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_graph::graph::{find_all_definitions, Direction, GraphTraverser, TraversalOptions};
use codegraph_store::Store;

fn temp_db_path(test_name: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "codegraph-graph-traversal-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: i64,
    end_line: i64,
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        file_path: file_path.to_string(),
        language,
        start_line,
        end_line,
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

fn edge(source: &str, target: &str, kind: EdgeKind, line: Option<i64>, col: Option<i64>) -> Edge {
    Edge {
        id: None,
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line,
        col,
        provenance: None,
    }
}

const APP_FILE: &str = "file:src/app.ts";
const MATH_FILE: &str = "file:src/math.ts";
const GREETER_FILE: &str = "file:tools/greeter.py";
const IMPORT: &str = "import:4ec14e7f870d20bf811e565c9993468c";
const RUN_DEMO: &str = "function:60629aa3876961b8bd3c07c43bbe6a37";
const ADD: &str = "function:cce15011e0125d59f6bef014ae79c04f";
const COUNTER: &str = "class:7ac92ae0dce4208dfad9148e025726c4";
const COUNTER_VALUE: &str = "method:89ae38f0afd5d9ed9a7eb3286a91a590";
const INCREMENT: &str = "method:f501ba98441869bd251c636e30d31e3d";
const GREETER: &str = "class:7ea38da25781893f519b36a80bfe6fb6";
const GREETER_INIT: &str = "method:d9785bc27131eb51e96712a75cf8b935";
const GREET: &str = "method:3c01f33894e563f457be2f29fffca19c";
const MAKE_GREETING: &str = "function:8a6f7270b7e9d62aee91dd9e70f126f3";

fn mini_nodes() -> Vec<Node> {
    use Language::{Python, TypeScript};
    use NodeKind::{Class, File, Function, Import, Method};
    vec![
        node(
            APP_FILE,
            File,
            "app.ts",
            "src/app.ts",
            "src/app.ts",
            TypeScript,
            1,
            10,
        ),
        node(
            IMPORT,
            Import,
            "./math",
            "./math",
            "src/app.ts",
            TypeScript,
            1,
            1,
        ),
        node(
            RUN_DEMO,
            Function,
            "runDemo",
            "runDemo",
            "src/app.ts",
            TypeScript,
            3,
            7,
        ),
        node(
            MATH_FILE,
            File,
            "math.ts",
            "src/math.ts",
            "src/math.ts",
            TypeScript,
            1,
            13,
        ),
        node(ADD, Function, "add", "add", "src/math.ts", TypeScript, 1, 3),
        node(
            COUNTER,
            Class,
            "Counter",
            "Counter",
            "src/math.ts",
            TypeScript,
            5,
            12,
        ),
        node(
            COUNTER_VALUE,
            Method,
            "value",
            "Counter::value",
            "src/math.ts",
            TypeScript,
            6,
            6,
        ),
        node(
            INCREMENT,
            Method,
            "increment",
            "Counter::increment",
            "src/math.ts",
            TypeScript,
            8,
            11,
        ),
        node(
            GREETER_FILE,
            File,
            "greeter.py",
            "tools/greeter.py",
            "tools/greeter.py",
            Python,
            1,
            12,
        ),
        node(
            GREETER,
            Class,
            "Greeter",
            "Greeter",
            "tools/greeter.py",
            Python,
            1,
            6,
        ),
        node(
            GREETER_INIT,
            Method,
            "__init__",
            "Greeter::__init__",
            "tools/greeter.py",
            Python,
            2,
            3,
        ),
        node(
            GREET,
            Method,
            "greet",
            "Greeter::greet",
            "tools/greeter.py",
            Python,
            5,
            6,
        ),
        node(
            MAKE_GREETING,
            Function,
            "make_greeting",
            "make_greeting",
            "tools/greeter.py",
            Python,
            9,
            11,
        ),
    ]
}

/// The resolved + contains edges byte-mirrored from `reference/golden/mini/edges.json`.
fn mini_edges() -> Vec<Edge> {
    use EdgeKind::{Calls, Contains, Imports, Instantiates};
    vec![
        edge(APP_FILE, RUN_DEMO, Calls, Some(9), Some(0)),
        edge(APP_FILE, IMPORT, Imports, Some(1), Some(0)),
        edge(MAKE_GREETING, GREET, Calls, Some(11), Some(11)),
        edge(MAKE_GREETING, GREETER, Instantiates, Some(10), Some(14)),
        edge(INCREMENT, ADD, Calls, Some(9), Some(17)),
        edge(APP_FILE, ADD, Imports, Some(1), Some(18)),
        edge(RUN_DEMO, COUNTER, Instantiates, Some(4), Some(18)),
        edge(RUN_DEMO, INCREMENT, Calls, Some(5), Some(2)),
        edge(RUN_DEMO, ADD, Calls, Some(5), Some(20)),
        edge(RUN_DEMO, INCREMENT, Calls, Some(6), Some(9)),
        edge(APP_FILE, COUNTER, Imports, Some(1), Some(9)),
        edge(COUNTER, COUNTER_VALUE, Contains, None, None),
        edge(COUNTER, INCREMENT, Contains, None, None),
        edge(GREETER, GREET, Contains, None, None),
        edge(GREETER, GREETER_INIT, Contains, None, None),
        edge(APP_FILE, RUN_DEMO, Contains, None, None),
        edge(APP_FILE, IMPORT, Contains, None, None),
        edge(MATH_FILE, COUNTER, Contains, None, None),
        edge(MATH_FILE, ADD, Contains, None, None),
        edge(GREETER_FILE, GREETER, Contains, None, None),
        edge(GREETER_FILE, MAKE_GREETING, Contains, None, None),
    ]
}

fn mini_store(test_name: &str) -> Store {
    let mut store = Store::open(&temp_db_path(test_name)).expect("open temp store");
    store.upsert_nodes(&mini_nodes()).expect("insert nodes");
    store.insert_edges(&mini_edges()).expect("insert edges");
    store
}

fn id_set<I: IntoIterator<Item = String>>(ids: I) -> BTreeSet<String> {
    ids.into_iter().collect()
}

#[test]
fn callers_returns_incoming_call_reference_import_sources() {
    let store = mini_store("callers");
    let traverser = GraphTraverser::new(&store);

    let callers = traverser.get_callers(ADD, 1).expect("callers");
    let got = id_set(callers.iter().map(|c| c.node.id.clone()));

    // RUN_DEMO + INCREMENT call add; APP_FILE imports add.
    let want = id_set([RUN_DEMO, INCREMENT, APP_FILE].map(str::to_string));
    assert_eq!(got, want, "callers(add) mismatch");
}

#[test]
fn callees_returns_outgoing_call_reference_import_targets() {
    let store = mini_store("callees");
    let traverser = GraphTraverser::new(&store);

    let callees = traverser.get_callees(RUN_DEMO, 1).expect("callees");
    let got = id_set(callees.iter().map(|c| c.node.id.clone()));

    // runDemo calls increment (x2, deduped) and add; instantiates Counter is
    // excluded (not in the calls/references/imports callee kinds).
    let want = id_set([INCREMENT, ADD].map(str::to_string));
    assert_eq!(got, want, "callees(runDemo) mismatch");
}

#[test]
fn impact_depth_2_matches_upstream_transitive_set() {
    let store = mini_store("impact");
    let traverser = GraphTraverser::new(&store);

    let impact = traverser.get_impact_radius(ADD, 2).expect("impact");
    let got = id_set(impact.nodes.keys().cloned());

    // Upstream trace: add -> {increment, app.ts, runDemo} at depth 1; their
    // dependents (runDemo<-app.ts, increment<-runDemo) are already in the set.
    let want = id_set([ADD, INCREMENT, APP_FILE, RUN_DEMO].map(str::to_string));
    assert_eq!(got, want, "impact(add, depth=2) mismatch");
}

#[test]
fn impact_on_container_pulls_children_then_their_dependents() {
    let store = mini_store("impact-container");
    let traverser = GraphTraverser::new(&store);

    // Counter is a container: its `contains` children (value, increment) join
    // the set at the same depth, then increment's dependents (runDemo) follow.
    let impact = traverser.get_impact_radius(COUNTER, 2).expect("impact");
    let got = id_set(impact.nodes.keys().cloned());

    let want = id_set([COUNTER, COUNTER_VALUE, INCREMENT, RUN_DEMO, APP_FILE].map(str::to_string));
    assert_eq!(got, want, "impact(Counter, depth=2) mismatch");
}

#[test]
fn node_name_ambiguity_returns_all_overloads() {
    let mut store = Store::open(&temp_db_path("ambiguity")).expect("open store");
    // Two distinct definitions sharing the bare name `execute`.
    let overloads = vec![
        node(
            "method:execute_a",
            NodeKind::Method,
            "execute",
            "ServiceA::execute",
            "src/a.ts",
            Language::TypeScript,
            10,
            20,
        ),
        node(
            "method:execute_b",
            NodeKind::Method,
            "execute",
            "ServiceB::execute",
            "src/b.ts",
            Language::TypeScript,
            30,
            40,
        ),
    ];
    store.upsert_nodes(&overloads).expect("insert overloads");

    let found = find_all_definitions(&store, "execute").expect("find_all_definitions");
    let got = id_set(found.iter().map(|n| n.id.clone()));
    let want = id_set(["method:execute_a", "method:execute_b"].map(str::to_string));
    assert_eq!(got, want, "ambiguous name must return all overloads");
}

#[test]
fn cyclic_graph_terminates_and_does_not_hang() {
    let mut store = Store::open(&temp_db_path("cycle")).expect("open store");
    let a = node(
        "function:cycle_a",
        NodeKind::Function,
        "a",
        "a",
        "src/c.ts",
        Language::TypeScript,
        1,
        2,
    );
    let b = node(
        "function:cycle_b",
        NodeKind::Function,
        "b",
        "b",
        "src/c.ts",
        Language::TypeScript,
        3,
        4,
    );
    store.upsert_nodes(&[a, b]).expect("insert cycle nodes");
    // A -> B -> A call cycle.
    store
        .insert_edges(&[
            edge(
                "function:cycle_a",
                "function:cycle_b",
                EdgeKind::Calls,
                Some(1),
                Some(0),
            ),
            edge(
                "function:cycle_b",
                "function:cycle_a",
                EdgeKind::Calls,
                Some(3),
                Some(0),
            ),
        ])
        .expect("insert cycle edges");

    let traverser = GraphTraverser::new(&store);
    let start = std::time::Instant::now();

    // High depths that would diverge without a visited set.
    let callers = traverser
        .get_callers("function:cycle_a", 1000)
        .expect("callers");
    let callees = traverser
        .get_callees("function:cycle_a", 1000)
        .expect("callees");
    let impact = traverser
        .get_impact_radius("function:cycle_a", 1000)
        .expect("impact");
    let bfs = traverser
        .traverse_bfs("function:cycle_a", &TraversalOptions::default())
        .expect("bfs");
    let dfs = traverser
        .traverse_dfs(
            "function:cycle_a",
            &TraversalOptions {
                direction: Direction::Outgoing,
                ..TraversalOptions::default()
            },
        )
        .expect("dfs");

    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "cyclic traversal must terminate quickly, took {elapsed:?}"
    );

    // Bounded result sets: only the two cycle nodes can ever appear.
    assert!(callers.len() <= 2, "callers bounded on cycle");
    assert!(callees.len() <= 2, "callees bounded on cycle");
    assert!(impact.nodes.len() <= 2, "impact bounded on cycle");
    assert_eq!(
        id_set(bfs.nodes.keys().cloned()),
        id_set(["function:cycle_a", "function:cycle_b"].map(str::to_string)),
        "bfs visits both cycle nodes exactly once"
    );
    assert_eq!(
        id_set(dfs.nodes.keys().cloned()),
        id_set(["function:cycle_a", "function:cycle_b"].map(str::to_string)),
        "dfs visits both cycle nodes exactly once"
    );
}

#[test]
fn bfs_outgoing_from_file_discovers_contained_structure_first() {
    let store = mini_store("bfs");
    let traverser = GraphTraverser::new(&store);

    let graph = traverser
        .traverse_bfs(MATH_FILE, &TraversalOptions::default())
        .expect("bfs");

    // math.ts contains add + Counter; Counter contains value + increment.
    assert!(graph.nodes.contains_key(ADD));
    assert!(graph.nodes.contains_key(COUNTER));
    assert!(graph.nodes.contains_key(INCREMENT));
    assert!(graph.nodes.contains_key(COUNTER_VALUE));
}

#[test]
fn ancestors_and_children_follow_contains_edges() {
    let store = mini_store("ancestors");
    let traverser = GraphTraverser::new(&store);

    let ancestors = traverser.get_ancestors(INCREMENT).expect("ancestors");
    let ancestor_ids: Vec<&str> = ancestors.iter().map(|n| n.id.as_str()).collect();
    // increment is contained by Counter, which is contained by math.ts.
    assert_eq!(ancestor_ids, vec![COUNTER, MATH_FILE]);

    let children = traverser.get_children(COUNTER).expect("children");
    let child_ids = id_set(children.iter().map(|n| n.id.clone()));
    assert_eq!(
        child_ids,
        id_set([COUNTER_VALUE, INCREMENT].map(str::to_string))
    );
}

#[test]
fn missing_start_node_returns_empty_subgraph() {
    let store = mini_store("missing");
    let traverser = GraphTraverser::new(&store);

    let graph = traverser
        .traverse_bfs("function:does_not_exist", &TraversalOptions::default())
        .expect("bfs");
    assert!(graph.nodes.is_empty());
    assert!(graph.edges.is_empty());
    assert!(graph.roots.is_empty());
}

#[test]
fn find_path_returns_shortest_outgoing_path() {
    let store = mini_store("path");
    let traverser = GraphTraverser::new(&store);

    let path = traverser
        .find_path(RUN_DEMO, ADD, &[EdgeKind::Calls])
        .expect("find_path")
        .expect("path exists");
    assert_eq!(path.first().unwrap().node.id, RUN_DEMO);
    assert_eq!(path.last().unwrap().node.id, ADD);
}

fn file_record(path: &str) -> codegraph_core::types::FileRecord {
    codegraph_core::types::FileRecord {
        path: path.to_string(),
        content_hash: "h".to_string(),
        language: Language::TypeScript,
        size: 1,
        modified_at: 1,
        indexed_at: 1,
        node_count: 1,
        errors: Vec::new(),
    }
}

fn cycle_node(id: &str, name: &str, file_path: &str) -> Node {
    node(
        id,
        NodeKind::Function,
        name,
        name,
        file_path,
        Language::TypeScript,
        1,
        2,
    )
}

#[test]
fn find_circular_dependencies_reports_cross_file_cycle() {
    let mut store = Store::open(&temp_db_path("circ-cycle")).expect("open store");
    for f in ["src/a.ts", "src/b.ts"] {
        store.upsert_file(&file_record(f)).expect("insert file");
    }
    store
        .upsert_nodes(&[
            cycle_node("function:fa", "a", "src/a.ts"),
            cycle_node("function:fb", "b", "src/b.ts"),
        ])
        .expect("insert nodes");
    store
        .insert_edges(&[
            edge(
                "function:fa",
                "function:fb",
                EdgeKind::Calls,
                Some(1),
                Some(0),
            ),
            edge(
                "function:fb",
                "function:fa",
                EdgeKind::Calls,
                Some(1),
                Some(0),
            ),
        ])
        .expect("insert edges");

    let traverser = GraphTraverser::new(&store);
    let cycles = traverser.find_circular_dependencies().expect("cycles");

    assert_eq!(cycles.len(), 1, "exactly one cycle");
    // Files iterate sorted: a.ts first → a→b→(a in stack) → cycle [a, b].
    assert_eq!(
        cycles[0],
        vec!["src/a.ts".to_string(), "src/b.ts".to_string()]
    );
}

#[test]
fn find_circular_dependencies_acyclic_is_empty() {
    let mut store = Store::open(&temp_db_path("circ-acyclic")).expect("open store");
    for f in ["src/a.ts", "src/b.ts"] {
        store.upsert_file(&file_record(f)).expect("insert file");
    }
    store
        .upsert_nodes(&[
            cycle_node("function:fa", "a", "src/a.ts"),
            cycle_node("function:fb", "b", "src/b.ts"),
        ])
        .expect("insert nodes");
    store
        .insert_edges(&[edge(
            "function:fa",
            "function:fb",
            EdgeKind::Calls,
            Some(1),
            Some(0),
        )])
        .expect("insert edges");

    let traverser = GraphTraverser::new(&store);
    let cycles = traverser.find_circular_dependencies().expect("cycles");
    assert!(cycles.is_empty(), "acyclic graph has no cycles");
}
