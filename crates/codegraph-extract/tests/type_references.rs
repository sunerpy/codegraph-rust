//! A declaration's annotated types are `references` from it (G3, upstream
//! `TYPE_ANNOTATION_LANGUAGES`): parameter and return types of functions and
//! methods, and the other type positions each language's own path reads.
//! Built-in and primitive types name no project symbol and are skipped.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// `(from, type)` for every `references` ref in `source`, sorted.
fn type_refs(path: &str, source: &str, language: Language) -> Vec<(String, String)> {
    let result = extract_source(path, source, Some(language));
    let name_of = |id: &str| {
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| id.to_string(), |node| node.name.clone())
    };
    let mut refs: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::References)
        .map(|r| (name_of(&r.from_node_id), r.reference_name.clone()))
        .collect();
    refs.sort();
    refs
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn rust_parameter_and_return_types_are_references() {
    let source = "pub fn build(w: Widget, s: &shop::Store, items: Vec<Item>, n: i32, name: &str) -> Result<Out, Error> {\n    todo!()\n}\n\npub struct Shape;\n\nimpl Shape {\n    pub fn grow(&self, by: Scale) -> f64 {\n        0.0\n    }\n}\n";
    assert_eq!(
        type_refs("src/build.rs", source, Language::Rust),
        pairs(&[
            ("build", "Error"),
            ("build", "Item"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Store"),
            ("build", "Vec"),
            ("build", "Widget"),
            ("grow", "Scale"),
        ])
    );
}

/// Go's builtin types (`int`, `error`, ...) are identifiers in its grammar and
/// are skipped by name; a method's receiver is not a parameter type.
#[test]
fn go_parameter_and_result_types_are_references() {
    let source = "package shop\n\nfunc Build(w Widget, s *store.Store, n int, items []Item) (Out, error) {\n\treturn Out{}, nil\n}\n\nfunc (s *Server) Handle(r Request) Response {\n\treturn Response{}\n}\n";
    assert_eq!(
        type_refs("shop/build.go", source, Language::Go),
        pairs(&[
            ("Build", "Item"),
            ("Build", "Out"),
            ("Build", "Store"),
            ("Build", "Widget"),
            ("Handle", "Request"),
            ("Handle", "Response"),
        ])
    );
}
