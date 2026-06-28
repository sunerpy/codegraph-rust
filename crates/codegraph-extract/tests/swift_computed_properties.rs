use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

fn extract(src: &str) -> ExtractionResult {
    extract_source("Sample.swift", src, Some(Language::Swift))
}

fn has_node(result: &ExtractionResult, kind: NodeKind, name: &str) -> bool {
    result
        .nodes
        .iter()
        .any(|node| node.kind == kind && node.name == name)
}

fn node<'a>(
    result: &'a ExtractionResult,
    kind: NodeKind,
    name: &str,
) -> &'a codegraph_core::types::Node {
    result
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind} {name}; nodes={:#?}", result.nodes))
}

#[test]
fn swift_computed_property_body_becomes_property_with_getter_calls() {
    // Given a SwiftUI computed `var body` whose getter calls a subview,
    // When extracted,
    // Then `body` is a property node and the getter's call is attributed to it.
    let result = extract(
        "struct ContentView {\n    var body: some View {\n        Text(\"hi\")\n    }\n}\n",
    );
    let body = node(&result, NodeKind::Property, "body");
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls
                && r.reference_name == "Text"
                && r.from_node_id == body.id),
        "getter call Text() must attribute to the body property; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn swift_stored_property_is_not_its_own_node() {
    // Given a stored property with an initializer,
    // When extracted,
    // Then it stays attached to the enclosing type (no standalone property node).
    let result = extract("struct V {\n    var stored = 1\n}\n");
    assert!(
        !has_node(&result, NodeKind::Property, "stored"),
        "stored property must not become its own node; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn swift_static_let_property_is_unchanged() {
    // Given a static stored constant,
    // When extracted,
    // Then it is not surfaced as a standalone property node (stored path intact).
    let result = extract("struct V {\n    static let shared = 2\n}\n");
    assert!(
        !has_node(&result, NodeKind::Property, "shared"),
        "static stored constant must not become its own node; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn swift_protocol_property_requirement_becomes_property() {
    // Given a protocol property requirement `var x: Int { get }`,
    // When extracted,
    // Then `x` is surfaced as a property node.
    let result = extract("protocol P {\n    var x: Int { get }\n}\n");
    assert!(
        has_node(&result, NodeKind::Property, "x"),
        "protocol property requirement must be a property node; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn swift_getter_local_binding_is_not_a_field() {
    // Given a computed property whose getter declares a local `let`,
    // When extracted,
    // Then the local is NOT mis-noded as a field on the type.
    let result = extract(
        "struct V {\n    var total: Int {\n        let local = compute()\n        return local\n    }\n}\n",
    );
    assert!(
        has_node(&result, NodeKind::Property, "total"),
        "computed total must be a property node; nodes={:#?}",
        result.nodes
    );
    assert!(
        !has_node(&result, NodeKind::Field, "local"),
        "getter-local binding must not be a field; nodes={:#?}",
        result.nodes
    );
    assert!(
        !has_node(&result, NodeKind::Property, "local"),
        "getter-local binding must not be a standalone property; nodes={:#?}",
        result.nodes
    );
}
