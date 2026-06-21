use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_source};
use serde::Deserialize;
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
    signature: Option<String>,
}

#[test]
fn python_mini_matches_real_upstream_golden_node_ids() {
    let file = "tools/greeter.py";
    let source = fs::read_to_string(format!("{MINI_ROOT}/{file}")).unwrap();
    let result = extract_source(file, &source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let golden = python_golden_nodes();
    let mut expected_ids = golden
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    let mut actual_ids = result
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    expected_ids.sort_unstable();
    actual_ids.sort_unstable();
    assert_eq!(
        actual_ids, expected_ids,
        "python golden node ids must be byte-equal"
    );
    assert!(actual_ids.contains(&"file:tools/greeter.py"));

    for expected in &golden {
        let actual = result
            .nodes
            .iter()
            .find(|node| node.id == expected.id)
            .unwrap_or_else(|| panic!("missing golden python node {}", expected.id));
        assert_eq!(actual.kind, expected.kind, "kind for {}", expected.id);
        assert_eq!(actual.name, expected.name, "name for {}", expected.id);
        assert_eq!(
            actual.qualified_name, expected.qualified_name,
            "qualified name for {}",
            expected.id
        );
        assert_eq!(
            actual.file_path, expected.file_path,
            "file path for {}",
            expected.id
        );
        assert_eq!(
            actual.language, expected.language,
            "language for {}",
            expected.id
        );
        assert_eq!(
            actual.start_line, expected.start_line,
            "start line for {}",
            expected.id
        );
        assert_eq!(
            actual.signature, expected.signature,
            "signature for {}",
            expected.id
        );
    }
    println!("python golden-match output: ids={actual_ids:?}");
}

#[test]
fn python_staticmethod_decorated_extracts_without_panic() {
    // Regression guard: PythonSpec::is_static previously read the decorator via
    // node_text(prev, "") (empty source), panicking on out-of-range byte slice
    // for any `@staticmethod`-decorated def. is_static now receives the real
    // source; the result must match the golden (is_static, # docstring stripped).
    let source = "# Decorated function docs.\n@staticmethod\ndef helper(x):\n    return x\n";
    let result = extract_source("src/deco.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let helper = result
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper node");
    assert!(helper.is_static, "@staticmethod must yield is_static=true");
    assert_eq!(
        helper.docstring.as_deref(),
        Some("Decorated function docs.")
    );
}

#[test]
fn rust_extracts_struct_trait_impl_pub_async_fn_and_use() {
    // Upstream rust.ts lines 35-56 define function/trait/struct/use node types.
    let source = r#"
use crate::helpers::{make_helper, Helper};

pub trait Greeter { fn greet(&self, name: &str) -> String; }
pub struct ConsoleGreeter { prefix: String }

impl Greeter for ConsoleGreeter {
    fn greet(&self, name: &str) -> String { make_helper(name) }
}

impl ConsoleGreeter {
    pub async fn connect() -> ConsoleGreeter { ConsoleGreeter { prefix: String::new() } }
}
"#;
    let result = extract_source("src/lib.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "crate");
    assert_node(&result.nodes, NodeKind::Trait, "Greeter");
    assert_node(&result.nodes, NodeKind::Struct, "ConsoleGreeter");
    assert_node(&result.nodes, NodeKind::Method, "greet");
    let connect = assert_node(&result.nodes, NodeKind::Method, "connect");
    assert!(connect.is_async, "connect should be async: {connect:#?}");
    assert_eq!(connect.visibility.as_deref(), Some("public"));
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Implements,
        "Greeter",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Imports,
        "crate::helpers::make_helper",
    );
}

#[test]
fn javascript_extracts_import_function_class_method_and_require_call() {
    // Upstream javascript.ts lines 4-14: ES import is an import; require remains a call.
    let source = r#"
import Widget from './widget';
export function run() { const fs = require('fs'); return helper(fs); }
class Panel { render() { return run(); } }
"#;
    let result = extract_source("src/app.js", source, Some(Language::JavaScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "./widget");
    assert_node(&result.nodes, NodeKind::Function, "run");
    assert_node(&result.nodes, NodeKind::Class, "Panel");
    assert_node(&result.nodes, NodeKind::Method, "render");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "require");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "run");
}

#[test]
fn tsx_and_jsx_extract_pascalcase_component_references() {
    // Upstream TSX/JSX routing reuses TS/JS extractors; walker handles JSX tags as references.
    let tsx = r#"
import { Card } from './Card';
export function App() { return <Card title="hi" />; }
"#;
    let tsx_result = extract_source("src/App.tsx", tsx, Some(Language::Tsx));
    assert!(tsx_result.errors.is_empty(), "{:?}", tsx_result.errors);
    assert_node(&tsx_result.nodes, NodeKind::Function, "App");
    assert_ref(
        &tsx_result.unresolved_references,
        EdgeKind::References,
        "Card",
    );

    let jsx = r#"
import Banner from './Banner';
export function Home() { return <Banner />; }
"#;
    let jsx_result = extract_source("src/Home.jsx", jsx, Some(Language::Jsx));
    assert!(jsx_result.errors.is_empty(), "{:?}", jsx_result.errors);
    assert_node(&jsx_result.nodes, NodeKind::Function, "Home");
    assert_ref(
        &jsx_result.unresolved_references,
        EdgeKind::References,
        "Banner",
    );
}

#[test]
fn go_extracts_package_import_struct_interface_methods_functions_and_calls() {
    // Upstream go.ts lines 41-57 map type_spec to struct/interface and imports to import_declaration.
    let source = r#"
package main
import "fmt"

type Greeter interface { Greet(name string) string }
type ConsoleGreeter struct { Prefix string }

func (g *ConsoleGreeter) Greet(name string) string { fmt.Println(name); return name }
func NewGreeter() *ConsoleGreeter { return &ConsoleGreeter{} }
func main() { NewGreeter().Greet("Ada") }
"#;
    let result = extract_source("cmd/main.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "fmt");
    assert_node(&result.nodes, NodeKind::Interface, "Greeter");
    assert_node(&result.nodes, NodeKind::Struct, "ConsoleGreeter");
    assert_node(&result.nodes, NodeKind::Method, "Greet");
    let factory = assert_node(&result.nodes, NodeKind::Function, "NewGreeter");
    assert_eq!(factory.return_type.as_deref(), Some("ConsoleGreeter"));
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Calls,
        "fmt.Println",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Calls,
        "NewGreeter().Greet",
    );
}

#[test]
fn batch_a_extensions_route_to_expected_languages() {
    assert_eq!(detect_language("lib.rs"), Language::Rust);
    assert_eq!(detect_language("app.js"), Language::JavaScript);
    assert_eq!(detect_language("app.jsx"), Language::Jsx);
    assert_eq!(detect_language("app.tsx"), Language::Tsx);
    assert_eq!(detect_language("greeter.py"), Language::Python);
    assert_eq!(detect_language("main.go"), Language::Go);
}

fn python_golden_nodes() -> Vec<GoldenNode> {
    let all: Vec<GoldenNode> =
        serde_json::from_str(include_str!("../../../reference/golden/mini/nodes.json")).unwrap();
    all.into_iter()
        .filter(|node| node.language == Language::Python)
        .collect()
}

fn assert_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}; nodes={nodes:#?}"))
}

fn assert_ref(refs: &[codegraph_core::types::UnresolvedRef], kind: EdgeKind, name: &str) {
    assert!(
        refs.iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing {kind:?} {name}; refs={refs:#?}"
    );
}
