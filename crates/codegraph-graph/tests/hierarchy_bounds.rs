//! The type hierarchy's descendant bound, against a synthetic store — upstream
//! `__tests__/type-hierarchy.test.ts` "the descendant bound" (`v1.6.1`). A fan
//! wide enough to hit the cap would be hundreds of files to index for one
//! assertion; the property pinned is arithmetic, not extraction.

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_graph::hierarchy::{MAX_DESCENDANTS, build_type_hierarchy};
use codegraph_store::Store;

fn class(id: &str, name: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Class,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: format!("src/{name}.ts"),
        language: Language::TypeScript,
        start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: true,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: 1,
    }
}

/// A root class and `children` subclasses of it.
fn fan(children: usize) -> (tempfile::TempDir, Store, Node) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("graph.db")).expect("open store");
    let root = class("class:root", "Root");
    let mut nodes = vec![root.clone()];
    let mut edges = Vec::new();
    for i in 0..children {
        let child = class(&format!("class:c{i:04}"), &format!("Child{i:04}"));
        edges.push(Edge {
            id: None,
            source: child.id.clone(),
            target: root.id.clone(),
            kind: EdgeKind::Extends,
            metadata: None,
            line: Some(1),
            col: Some(0),
            provenance: None,
        });
        nodes.push(child);
    }
    store.upsert_nodes(&nodes).unwrap();
    store.insert_edges(&edges).unwrap();
    (dir, store, root)
}

#[test]
fn stays_unbounded_under_the_cap() {
    let (_dir, store, root) = fan(10);
    let hierarchy = build_type_hierarchy(&store, &root).unwrap();
    assert_eq!(hierarchy.descendants.len(), 10);
    assert_eq!(hierarchy.direct_subtypes, 10);
    assert!(!hierarchy.bounded);
}

#[test]
fn stops_materialising_rows_past_the_cap_but_keeps_the_direct_count_true() {
    let (_dir, store, root) = fan(MAX_DESCENDANTS + 37);
    let hierarchy = build_type_hierarchy(&store, &root).unwrap();
    assert_eq!(hierarchy.descendants.len(), MAX_DESCENDANTS);
    // The number of subtypes is not the number of rows, and says so.
    assert_eq!(hierarchy.direct_subtypes, MAX_DESCENDANTS + 37);
    assert!(hierarchy.bounded);
}
