use codegraph_core::types::{
    Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedRef,
};
use codegraph_extract::{ExtractOptions, extract_project, extract_source};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;

const MINI_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/codegraph-bench/fixtures/mini"
);

#[derive(Debug, Deserialize)]
struct GoldenNode {
    id: String,
    kind: NodeKind,
    name: String,
    qualified_name: String,
    file_path: String,
    language: Language,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<String>,
    is_exported: i64,
    is_async: i64,
    is_static: i64,
    is_abstract: i64,
    return_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoldenEdge {
    source: String,
    target: String,
    kind: EdgeKind,
    line: Option<i64>,
    col: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    source: String,
    target: String,
    kind: EdgeKindOrder,
    line: Option<i64>,
    col: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKindOrder {
    Calls,
    Contains,
    Imports,
    Instantiates,
    Other,
}

#[test]
fn matches_real_upstream_ts_golden_nodes() {
    let result = extract_ts_mini();
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let golden = golden_ts_nodes();

    assert_eq!(
        result.nodes.len(),
        golden.len(),
        "actual nodes: {:#?}",
        result.nodes
    );
    for expected in golden {
        let actual = result
            .nodes
            .iter()
            .find(|node| node.id == expected.id)
            .unwrap_or_else(|| panic!("missing golden node {}", expected.id));
        assert_node_matches(actual, &expected);
    }
    println!("TS golden nodes matched: {}", result.nodes.len());
}

#[test]
fn matches_real_upstream_ts_golden_edges_after_local_resolution() {
    let result = extract_ts_mini();
    let resolved = resolve_mini_edges(&result);
    let ts_ids = result
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let expected = golden_ts_edges(&ts_ids);
    let actual = resolved
        .iter()
        .filter(|edge| ts_ids.contains(&edge.source) && ts_ids.contains(&edge.target))
        .map(edge_key)
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
    println!("TS golden resolved edges matched: {}", actual.len());
}

#[test]
fn captures_ts_decorator_references_from_focused_fixture() {
    let source = r#"
function sealed(target: unknown) { return target; }
function memo(_target: unknown, _name: string) {}

@sealed
export class Box {
  @memo
  value(): number { return 1; }
}
"#;
    let result = extract_source("src/decorators.ts", source, Some(Language::TypeScript));
    let refs = result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::Decorates)
        .map(|reference| (reference.reference_name.as_str(), reference.line))
        .collect::<BTreeSet<_>>();
    assert!(refs.contains(&("sealed", 5)), "decorator refs: {refs:?}");
    assert!(refs.contains(&("memo", 7)), "decorator refs: {refs:?}");
}

#[test]
fn docstring_capture_matches_upstream_for_wrapped_declarations() {
    // Locks docstring equivalence with the upstream 1.0.1 (getPrecedingDocstring,
    // tree-sitter-helpers.ts; #780 / 0df92467). Each declaration's leading
    // comment must attach despite export/const wrapping, and the C-family
    // marker syntax must be stripped. Expected values captured byte-identical
    // from the upstream 1.0.1 on this exact source.
    let source = concat!(
        "/**\n * A widget.\n * Second line.\n */\n",
        "export class Widget {}\n\n",
        "/** Inline fn doc. */\n",
        "export function build(): number {\n  return 1;\n}\n\n",
        "// plain const arrow doc\n",
        "export const make = (): number => 2;\n\n",
        "// Non-exported arrow doc.\n",
        "const helper = (): number => 3;\n",
    );
    let result = extract_source("src/docstrings.ts", source, Some(Language::TypeScript));

    let docstring_for = |name: &str| -> Option<String> {
        result
            .nodes
            .iter()
            .find(|n| n.name == name)
            .and_then(|n| n.docstring.clone())
    };

    assert_eq!(
        docstring_for("Widget").as_deref(),
        Some("A widget.\nSecond line.")
    );
    assert_eq!(docstring_for("build").as_deref(), Some("Inline fn doc."));
    assert_eq!(
        docstring_for("make").as_deref(),
        Some("plain const arrow doc")
    );
    assert_eq!(
        docstring_for("helper").as_deref(),
        Some("Non-exported arrow doc.")
    );
}

#[test]
fn directory_extraction_is_deterministic_between_sequential_and_parallel() {
    let temp = std::env::temp_dir().join(format!(
        "codegraph-extract-determinism-{}",
        std::process::id()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).unwrap();
    }
    let src = temp.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..20 {
        fs::write(
            src.join(format!("file{i}.ts")),
            format!("export function f{i}(): number {{ return {i}; }}\n"),
        )
        .unwrap();
    }

    let sequential = extract_project(
        &temp,
        &ExtractOptions {
            parallel: false,
            ..ExtractOptions::default()
        },
    )
    .unwrap();
    let parallel = extract_project(&temp, &ExtractOptions::default()).unwrap();

    assert_eq!(canonical_result(&sequential), canonical_result(&parallel));
    println!(
        "deterministic extraction: nodes={} edges={} refs={}",
        sequential.nodes.len(),
        sequential.edges.len(),
        sequential.unresolved_references.len()
    );
    fs::remove_dir_all(&temp).unwrap();
}

fn extract_ts_mini() -> ExtractionResult {
    let mut result = ExtractionResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved_references: Vec::new(),
        errors: Vec::new(),
        duration_ms: 0,
    };
    for file in ["src/app.ts", "src/math.ts"] {
        let source = fs::read_to_string(format!("{MINI_ROOT}/{file}")).unwrap();
        let mut partial = extract_source(file, &source, Some(Language::TypeScript));
        result.nodes.append(&mut partial.nodes);
        result.edges.append(&mut partial.edges);
        result
            .unresolved_references
            .append(&mut partial.unresolved_references);
        result.errors.append(&mut partial.errors);
    }
    result
}

fn golden_ts_nodes() -> Vec<GoldenNode> {
    let all: Vec<GoldenNode> =
        serde_json::from_str(include_str!("../../../reference/golden/mini/nodes.json")).unwrap();
    all.into_iter()
        .filter(|node| node.language == Language::TypeScript)
        .collect()
}

fn golden_ts_edges(ts_ids: &BTreeSet<String>) -> BTreeSet<EdgeKey> {
    let all: Vec<GoldenEdge> =
        serde_json::from_str(include_str!("../../../reference/golden/mini/edges.json")).unwrap();
    all.into_iter()
        .filter(|edge| ts_ids.contains(&edge.source) && ts_ids.contains(&edge.target))
        .map(|edge| EdgeKey {
            source: edge.source,
            target: edge.target,
            kind: edge_kind_order(edge.kind),
            line: edge.line,
            col: edge.col,
        })
        .collect()
}

fn assert_node_matches(actual: &Node, expected: &GoldenNode) {
    assert_eq!(actual.kind, expected.kind, "kind for {}", expected.id);
    assert_eq!(actual.name, expected.name, "name for {}", expected.id);
    assert_eq!(
        actual.qualified_name, expected.qualified_name,
        "qualified_name for {}",
        expected.id
    );
    assert_eq!(
        actual.file_path, expected.file_path,
        "file_path for {}",
        expected.id
    );
    assert_eq!(
        actual.language, expected.language,
        "language for {}",
        expected.id
    );
    assert_eq!(
        actual.start_line, expected.start_line,
        "start_line for {}",
        expected.id
    );
    assert_eq!(
        actual.end_line, expected.end_line,
        "end_line for {}",
        expected.id
    );
    assert_eq!(
        actual.start_column, expected.start_column,
        "start_column for {}",
        expected.id
    );
    assert_eq!(
        actual.end_column, expected.end_column,
        "end_column for {}",
        expected.id
    );
    assert_eq!(
        actual.docstring, expected.docstring,
        "docstring for {}",
        expected.id
    );
    assert_eq!(
        actual.signature, expected.signature,
        "signature for {}",
        expected.id
    );
    assert_eq!(
        actual.visibility, expected.visibility,
        "visibility for {}",
        expected.id
    );
    assert_eq!(
        actual.is_exported,
        expected.is_exported == 1,
        "is_exported for {}",
        expected.id
    );
    assert_eq!(
        actual.is_async,
        expected.is_async == 1,
        "is_async for {}",
        expected.id
    );
    assert_eq!(
        actual.is_static,
        expected.is_static == 1,
        "is_static for {}",
        expected.id
    );
    assert_eq!(
        actual.is_abstract,
        expected.is_abstract == 1,
        "is_abstract for {}",
        expected.id
    );
    assert_eq!(
        actual.return_type, expected.return_type,
        "return_type for {}",
        expected.id
    );
}

fn resolve_mini_edges(result: &ExtractionResult) -> Vec<Edge> {
    let mut edges = result.edges.clone();
    let by_name = result
        .nodes
        .iter()
        .map(|node| (node.name.as_str(), node))
        .collect::<HashMap<_, _>>();
    let import_nodes = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Import)
        .map(|node| (node.name.as_str(), node))
        .collect::<HashMap<_, _>>();

    for reference in &result.unresolved_references {
        let target = match reference.reference_kind {
            EdgeKind::Imports => import_nodes
                .get(reference.reference_name.as_str())
                .copied()
                .or_else(|| by_name.get(reference.reference_name.as_str()).copied()),
            EdgeKind::Calls => {
                let simple = reference
                    .reference_name
                    .rsplit('.')
                    .next()
                    .unwrap_or(reference.reference_name.as_str());
                by_name.get(simple).copied()
            }
            EdgeKind::Instantiates | EdgeKind::References | EdgeKind::Decorates => {
                by_name.get(reference.reference_name.as_str()).copied()
            }
            _ => None,
        };
        if let Some(target) = target {
            edges.push(edge_from_ref(reference, &target.id));
        }
    }
    edges
}

fn edge_from_ref(reference: &UnresolvedRef, target: &str) -> Edge {
    Edge {
        id: None,
        source: reference.from_node_id.clone(),
        target: target.to_string(),
        kind: reference.reference_kind,
        metadata: None,
        line: Some(reference.line),
        col: Some(reference.col),
        provenance: None,
    }
}

fn edge_key(edge: &Edge) -> EdgeKey {
    EdgeKey {
        source: edge.source.clone(),
        target: edge.target.clone(),
        kind: edge_kind_order(edge.kind),
        line: edge.line,
        col: edge.col,
    }
}

fn edge_kind_order(kind: EdgeKind) -> EdgeKindOrder {
    match kind {
        EdgeKind::Calls => EdgeKindOrder::Calls,
        EdgeKind::Contains => EdgeKindOrder::Contains,
        EdgeKind::Imports => EdgeKindOrder::Imports,
        EdgeKind::Instantiates => EdgeKindOrder::Instantiates,
        _ => EdgeKindOrder::Other,
    }
}

fn canonical_result(result: &ExtractionResult) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut nodes = result
        .nodes
        .iter()
        .map(|node| {
            format!(
                "{}:{}:{}:{}:{}",
                node.id, node.kind, node.name, node.file_path, node.start_line
            )
        })
        .collect::<Vec<_>>();
    let mut edges = result
        .edges
        .iter()
        .map(|edge| format!("{}:{}:{}", edge.source, edge.target, edge.kind))
        .collect::<Vec<_>>();
    let mut refs = result
        .unresolved_references
        .iter()
        .map(|reference| {
            format!(
                "{}:{}:{}:{}:{}",
                reference.from_node_id,
                reference.reference_name,
                reference.reference_kind,
                reference.line,
                reference.col
            )
        })
        .collect::<Vec<_>>();
    nodes.sort();
    edges.sort();
    refs.sort();
    (nodes, edges, refs)
}
