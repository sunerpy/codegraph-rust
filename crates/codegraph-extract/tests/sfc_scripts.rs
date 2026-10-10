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
