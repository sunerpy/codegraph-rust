//! The unresolved references a sync retries through their receiver (W5-07):
//! a class-name call whose walk reads a changed head waits under its method's
//! tail, which the tail lookup caps, so it is found by its receiver instead.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;

fn file_node(file: &str) -> Node {
    Node {
        id: format!("file:{file}"),
        kind: NodeKind::File,
        name: file.to_string(),
        qualified_name: file.to_string(),
        file_path: file.to_string(),
        language: Language::Python,
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
        updated_at: 0,
    }
}

fn parked(name: &str) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: "file:a.py".to_string(),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        col: 0,
        candidates: None,
        file_path: "a.py".to_string(),
        language: Language::Python,
        is_function_ref: false,
        reference_subkind: None,
    }
}

#[test]
fn a_reference_is_found_through_its_receiver_and_nothing_else() {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-receiver-refs-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = Store::open(&path).expect("open store");
    store.upsert_nodes(&[file_node("a.py")]).expect("file node");
    store
        .insert_unresolved_refs(&[
            parked("User.create"),
            parked("User::create"),
            parked("User.create.save"),
            parked("UserX.create"),
            parked("User"),
            parked("Admin.User.create"),
            parked("Other.create"),
        ])
        .expect("park refs");
    let mut names: Vec<String> = store
        .unresolved_refs_by_receivers(&["User".to_string(), "Absent".to_string()])
        .expect("by receivers")
        .into_iter()
        .map(|reference| reference.reference_name)
        .collect();
    names.sort();
    assert_eq!(names, ["User.create", "User.create.save", "User::create"]);
    assert!(
        store
            .unresolved_refs_by_receivers(&[])
            .expect("no receivers")
            .is_empty()
    );
    let _ = std::fs::remove_file(&path);
}
