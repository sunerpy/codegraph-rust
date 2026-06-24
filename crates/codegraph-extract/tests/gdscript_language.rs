use codegraph_core::types::{EdgeKind, Language, NodeKind};
#[allow(unused_imports)]
use codegraph_extract::{detect_language, extract_source};

#[test]
fn gdscript_detects_extension() {
    assert_eq!(detect_language("scripts/player.gd"), Language::Gdscript);
    assert_eq!(detect_language("x.unknownext"), Language::Unknown);
}

#[test]
fn gdscript_extracts_top_level_function() {
    let source = "func ready():\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Function, "ready");
}

#[test]
fn gdscript_static_function_is_static() {
    let source = "static func make():\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    let node = result
        .nodes
        .iter()
        .find(|node| node.name == "make")
        .unwrap_or_else(|| panic!("missing node make; nodes={:#?}", result.nodes));
    assert!(node.is_static, "make should be static; node={node:#?}");
}

#[test]
fn gdscript_constructor_init_captured() {
    let source = "func _init():\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(
        result.nodes.iter().any(|node| node.name == "_init"
            && matches!(node.kind, NodeKind::Function | NodeKind::Method)),
        "missing _init Function/Method; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn gdscript_plain_call_emits_calls_ref() {
    let source = "func f():\n\tprint(\"hi\")\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "print");
}

#[test]
fn gdscript_malformed_func_no_panic() {
    let source = "func :\n\t@@@\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    let node_count = result.nodes.len();
    assert!(node_count < usize::MAX);
}

#[test]
fn gdscript_inner_class_with_method() {
    let source = "class Inner:\n\tfunc m():\n\t\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Inner");
    assert_node(&result.nodes, NodeKind::Method, "m");
}

#[test]
fn gdscript_inner_class_malformed_no_panic() {
    let source = "class Inner:\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    let node_count = result.nodes.len();
    assert!(node_count < usize::MAX);
}

#[test]
fn gdscript_named_enum_with_members() {
    let source = "enum Mode { FAST, SLOW }\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Enum, "Mode");
    assert_node(&result.nodes, NodeKind::EnumMember, "FAST");
    assert_node(&result.nodes, NodeKind::EnumMember, "SLOW");
}

#[test]
fn gdscript_anonymous_enum() {
    let source = "enum { A, B }\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(
        result.nodes.iter().any(|node| node.kind == NodeKind::Enum),
        "missing Enum node; nodes={:#?}",
        result.nodes
    );
    assert_node(&result.nodes, NodeKind::EnumMember, "A");
    assert_node(&result.nodes, NodeKind::EnumMember, "B");
}

#[test]
fn gdscript_enum_value_not_spurious_member() {
    let source = "enum E { X = SOME_CONST }\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::EnumMember, "X");
    assert!(
        !result
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::EnumMember && node.name == "SOME_CONST"),
        "spurious EnumMember SOME_CONST; nodes={:#?}",
        result.nodes
    );
    let member_count = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::EnumMember)
        .count();
    assert_eq!(
        member_count, 1,
        "expected exactly 1 EnumMember; nodes={:#?}",
        result.nodes
    );
}

#[allow(dead_code)]
fn assert_node(nodes: &[codegraph_core::types::Node], kind: NodeKind, name: &str) {
    assert!(
        nodes
            .iter()
            .any(|node| node.kind == kind && node.name == name),
        "missing {kind:?} {name}; nodes={nodes:#?}"
    );
}

#[allow(dead_code)]
fn assert_ref(refs: &[codegraph_core::types::UnresolvedRef], kind: EdgeKind, name: &str) {
    assert!(
        refs.iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing {kind:?} {name}; refs={refs:#?}"
    );
}
