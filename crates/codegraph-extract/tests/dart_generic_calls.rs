//! A Dart generic call names what it calls without its type arguments
//! (upstream #2375): `ref.read<Repo>(p)` calls `ref.read`, the name the
//! method is declared under. tree-sitter-dart parses the callee as an
//! `instantiation_expression` around that name; the type arguments stay
//! `references` of the caller.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

fn refs(source: &str, kind: EdgeKind) -> Vec<String> {
    let result = extract_source("lib/run.dart", source, Some(Language::Dart));
    let mut names: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == kind)
        .map(|r| r.reference_name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn a_generic_call_names_its_callee_without_type_arguments() {
    let source = "void run(Ref ref, BuildContext context) {\n  ref.read<Repo>(repoProvider).load();\n  BlocProvider.of<Counter>(context).increment();\n  final p = Provider<int>((ref) => 0);\n  identity<int>(3);\n  fetch<List<Item>>();\n  ref.watch<Map<String, int>>(p);\n}\n";
    assert_eq!(
        refs(source, EdgeKind::Calls),
        [
            "BlocProvider.of",
            "Provider",
            "fetch",
            "identity",
            "increment",
            "load",
            "ref.read",
            "ref.watch",
        ]
    );
    let references = refs(source, EdgeKind::References);
    for type_argument in ["Counter", "Item", "List", "Map", "Repo"] {
        assert!(
            references.iter().any(|name| name == type_argument),
            "{type_argument} in {references:?}"
        );
    }
}
