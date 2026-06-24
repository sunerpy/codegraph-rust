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

#[test]
fn gdscript_const_is_constant() {
    let source = "const C = 1\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Constant, "C");
}

#[test]
fn gdscript_var_is_variable() {
    let source = "var hp = 10\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Variable, "hp");
}

#[test]
fn gdscript_export_var_is_variable() {
    let source = "@export var speed: float\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Variable, "speed");
}

#[test]
fn gdscript_onready_var_is_variable() {
    let source = "@onready var node_ref = 1\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Variable, "node_ref");
}

#[test]
fn gdscript_var_const_counts() {
    let source = "const C = 1\nvar a = 1\nvar b = 2\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    let constant_count = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Constant)
        .count();
    let variable_count = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Variable)
        .count();
    assert_eq!(
        constant_count, 1,
        "expected exactly 1 Constant; nodes={:#?}",
        result.nodes
    );
    assert_eq!(
        variable_count, 2,
        "expected exactly 2 Variable; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn gdscript_malformed_var_no_panic() {
    let source = "var \n@@@\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    let node_count = result.nodes.len();
    assert!(node_count < usize::MAX);
}

#[test]
fn gdscript_extends_type() {
    let source = "extends Node\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_ref(&result.unresolved_references, EdgeKind::Extends, "Node");
}

#[test]
fn gdscript_extends_string_path() {
    let source = "extends \"res://base.gd\"\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Extends,
        "res://base.gd",
    );
}

#[test]
fn gdscript_inner_class_extends() {
    let source = "class Inner extends Node2D:\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_ref(&result.unresolved_references, EdgeKind::Extends, "Node2D");
}

#[test]
fn gdscript_class_name_is_class() {
    let source = "class_name Player\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Player");
}

#[test]
fn gdscript_class_name_func_stays_function() {
    let source = "class_name Player\nfunc ready():\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Player");
    assert_node(&result.nodes, NodeKind::Function, "ready");
    assert!(
        !result
            .nodes
            .iter()
            .any(|node| node.name == "ready" && node.kind == NodeKind::Method),
        "ready must NOT be a Method (class_name not pushed on stack); nodes={:#?}",
        result.nodes
    );
}

#[test]
fn gdscript_no_extends_no_ref() {
    let source = "func f():\n\tpass\n";
    let result = extract_source("scripts/x.gd", source, Some(Language::Gdscript));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_kind == EdgeKind::Extends),
        "expected ZERO Extends refs; refs={:#?}",
        result.unresolved_references
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
