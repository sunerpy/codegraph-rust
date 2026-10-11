//! A single-file component's `<script>` blocks are extracted by the TypeScript
//! (or JavaScript) extractor and joined to the component: symbols at their
//! file lines, and the calls written inside them (G8, upstream
//! vue-extractor.ts since #66).

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

fn calls_from(result: &ExtractionResult, from_name: &str) -> Vec<String> {
    let ids: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.name == from_name)
        .map(|n| n.id.as_str())
        .collect();
    let mut calls: Vec<String> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && ids.contains(&r.from_node_id.as_str()))
        .map(|r| r.reference_name.clone())
        .collect();
    calls.sort();
    calls
}

const APP_VUE: &str = "<template>\n  <div @click=\"go\">{{ msg }}</div>\n</template>\n\n<script setup lang=\"ts\">\nimport { helper } from './util';\n\nconst msg = helper();\n\nfunction go() {\n  run();\n  return helper();\n}\n</script>\n";

#[test]
fn a_vue_script_is_extracted_by_the_typescript_extractor() {
    let result = extract_source("src/App.vue", APP_VUE, None);
    let go = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "go")
        .expect("the script's function is a node");
    assert_eq!((go.start_line, go.end_line), (10, 13), "at its file lines");
    assert_eq!(go.language, Language::Vue);
    assert_eq!(calls_from(&result, "go"), ["helper", "run"]);
    assert!(
        result.nodes.iter().any(|n| n.name == "msg"),
        "a top-level constant is a node"
    );
    let imports: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .map(|r| (r.reference_name.as_str(), r.line))
        .collect();
    assert!(imports.contains(&("./util", 6)), "{imports:?}");
}

fn node_id(result: &ExtractionResult, kind: NodeKind, name: &str) -> String {
    result
        .nodes
        .iter()
        .find(|n| n.kind == kind && n.name == name)
        .unwrap_or_else(|| panic!("no {kind:?} {name}"))
        .id
        .clone()
}

fn refs_from(result: &ExtractionResult, from: &str, kind: EdgeKind) -> Vec<String> {
    let mut names: Vec<String> = result
        .unresolved_references
        .iter()
        .filter(|r| r.from_node_id == from && r.reference_kind == kind)
        .map(|r| r.reference_name.clone())
        .collect();
    names.sort();
    names
}

fn parents_of(result: &ExtractionResult, id: &str) -> Vec<String> {
    result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains && e.target == id)
        .map(|e| e.source.clone())
        .collect()
}

/// G9 / upstream #2268: the SFC has one file node, holding the component;
/// the component holds what the script holds at its top level, and a
/// `<script setup>`'s top-level calls (a constant's initializer's included)
/// are the component's, while its imports stay with the file.
#[test]
fn a_vue_setup_script_folds_into_its_component() {
    let result = extract_source("src/App.vue", APP_VUE, None);
    let file = node_id(&result, NodeKind::File, "App.vue");
    assert_eq!(file, "file:src/App.vue");
    let component = node_id(&result, NodeKind::Component, "App");
    assert_eq!(parents_of(&result, &component), std::slice::from_ref(&file));
    let go = node_id(&result, NodeKind::Function, "go");
    assert_eq!(
        parents_of(&result, &go),
        std::slice::from_ref(&component),
        "one parent each"
    );
    assert_eq!(refs_from(&result, &component, EdgeKind::Calls), ["helper"]);
    // The module and the name it binds, both the file's.
    assert_eq!(
        refs_from(&result, &file, EdgeKind::Imports),
        ["./util", "helper"]
    );
    for reference in &result.unresolved_references {
        assert!(
            result.nodes.iter().any(|n| n.id == reference.from_node_id),
            "{reference:?} comes from a node of the file"
        );
    }
}

/// A Svelte instance script runs once per component instance, so its
/// top-level `doSetup();` is the component's call; a module script's
/// top-level call stays with the file.
#[test]
fn a_svelte_instance_script_call_is_the_components() {
    let source = "<script context=\"module\">\n  prepare();\n</script>\n\n<script>\n  import { doSetup } from './setup';\n  doSetup();\n</script>\n\n<p>hi</p>\n";
    let result = extract_source("src/Widget.svelte", source, None);
    let file = node_id(&result, NodeKind::File, "Widget.svelte");
    let component = node_id(&result, NodeKind::Component, "Widget");
    assert_eq!(refs_from(&result, &component, EdgeKind::Calls), ["doSetup"]);
    assert_eq!(refs_from(&result, &file, EdgeKind::Calls), ["prepare"]);
    assert_eq!(
        refs_from(&result, &file, EdgeKind::Imports),
        ["./setup", "doSetup"]
    );
}
