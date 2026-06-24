use codegraph_core::types::{EdgeKind, Language, NodeKind};
#[allow(unused_imports)]
use codegraph_extract::{detect_language, extract_source};

#[test]
fn gdscript_detects_extension() {
    assert_eq!(detect_language("scripts/player.gd"), Language::Gdscript);
    assert_eq!(detect_language("x.unknownext"), Language::Unknown);
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
