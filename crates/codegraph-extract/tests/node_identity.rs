//! Same-line node identity (upstream v1.6.1 #1349).
//!
//! Same-kind, same-name declarations on one line used to share one node id, so
//! the later silently replaced the earlier in the store. The first keeps its
//! legacy id; a later distinct declaration appends `:{utf16Column}`.

use codegraph_core::node_id::{generate_node_id, utf16_column};
use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use codegraph_extract::extract_source;

fn extract(file: &str, source: &str, language: Language) -> ExtractionResult {
    let result = extract_source(file, source, Some(language));
    assert!(result.errors.is_empty(), "{file}: {:?}", result.errors);
    result
}

fn named<'a>(result: &'a ExtractionResult, name: &str, kind: NodeKind) -> Vec<&'a Node> {
    let mut nodes = result
        .nodes
        .iter()
        .filter(|node| node.name == name && node.kind == kind)
        .collect::<Vec<_>>();
    nodes.sort_by_key(|node| (node.start_line, node.start_column));
    nodes
}

#[test]
fn same_line_accessors_keep_distinct_identities_after_unicode() {
    let source = "class Point { /* é😀 */ get x() { return read(); } set x(v) { write(v); } }";
    let setter = utf16_column(source, source.find("set x").expect("setter"));
    for (file, language) in [
        ("point.ts", Language::TypeScript),
        ("point.tsx", Language::Tsx),
        ("point.js", Language::JavaScript),
        ("point.jsx", Language::Jsx),
    ] {
        let result = extract(file, source, language);
        let x = named(&result, "x", NodeKind::Method);
        assert_eq!(x.len(), 2, "{file}: {:#?}", result.nodes);
        assert_eq!(
            x[0].id,
            generate_node_id(file, NodeKind::Method, "x", 1),
            "{file}"
        );
        assert_eq!(x[1].id, format!("{}:{setter}", x[0].id), "{file}");
        let calls = result
            .unresolved_references
            .iter()
            .filter(|reference| reference.reference_kind == EdgeKind::Calls)
            .map(|reference| {
                (
                    reference.from_node_id.as_str(),
                    reference.reference_name.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![(x[0].id.as_str(), "read"), (x[1].id.as_str(), "write")],
            "{file}"
        );
        // No state leaks between files or repeated extractions.
        assert_eq!(
            extract(file, source, language).nodes,
            result.nodes,
            "{file}"
        );
    }
}

#[test]
fn distinct_lines_and_names_keep_their_legacy_ids() {
    let source = "export class Point {\n  get y() { return 1; }\n  set y(v: number) {}\n}\n";
    let result = extract("point.ts", source, Language::TypeScript);
    for node in result
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
    {
        assert_eq!(
            node.id,
            generate_node_id(
                &node.file_path,
                node.kind,
                &node.name,
                node.start_line as u32
            ),
            "{}",
            node.name
        );
    }
    assert_eq!(named(&result, "y", NodeKind::Method).len(), 2);
}

#[test]
fn repeated_same_line_liquid_and_cfml_declarations_survive() {
    let liquid = extract(
        "template.liquid",
        "é😀 {% render \"x\" %}{% render \"x\" %}{% assign v = 1 %}{% assign v = 2 %}",
        Language::Liquid,
    );
    for (name, kind) in [
        ("x", NodeKind::Component),
        ("x", NodeKind::Import),
        ("v", NodeKind::Variable),
    ] {
        let nodes = named(&liquid, name, kind);
        assert_eq!(nodes.len(), 2, "{kind:?} {name}: {:#?}", liquid.nodes);
        assert_ne!(nodes[0].id, nodes[1].id, "{kind:?} {name}");
        for node in nodes {
            assert!(
                liquid
                    .edges
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::Contains && edge.target == node.id),
                "{kind:?} {name} has no contains edge"
            );
        }
    }

    let cfml = extract(
        "Service.cfc",
        "<cfcomponent><!--- é😀 ---><cffunction name=\"x\"></cffunction><cffunction name=\"x\"></cffunction></cfcomponent>",
        Language::Cfml,
    );
    let methods = named(&cfml, "x", NodeKind::Method);
    assert_eq!(methods.len(), 2, "{:#?}", cfml.nodes);
    assert_ne!(methods[0].id, methods[1].id);
}
