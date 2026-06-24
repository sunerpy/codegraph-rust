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
