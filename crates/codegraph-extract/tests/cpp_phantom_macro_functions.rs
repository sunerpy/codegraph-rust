//! C++ extraction for upstream #946 (`f63e5db2`, the shape at `b635dd46`):
//! a class or struct annotated with a macro tree-sitter does not know
//! (`class MYLIB_EXPORT Foo : public Bar { … }`) reads as an elaborated type
//! specifier followed by a function definition named after the class (or its
//! base), spanning the whole body. That phantom function polluted callers and
//! impact. The ALL-CAPS export-macro shapes are recovered as classes before
//! this check runs (W4-CPP-01); what is left, a lowercase macro or one the
//! recovery does not read, mints no function.

use codegraph_core::types::{ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

/// `<kind> <qualified name>` for every symbol, file and imports aside.
fn symbols(result: &ExtractionResult) -> Vec<String> {
    result
        .nodes
        .iter()
        .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Import))
        .map(|node| format!("{} {}", node.kind.as_str(), node.qualified_name))
        .collect()
}

/// Upstream #946's cases: a macro-annotated class that inherits, and the
/// struct variant beside a genuine class with a base clause.
#[test]
fn a_macro_annotated_class_mints_no_phantom_function() {
    let header = "#pragma once\n#define MAPCORE_EXPORT __attribute__((visibility(\"default\")))\n\nclass DataProvider {\npublic:\n    virtual bool Request(void* param) = 0;\n};\n\nclass MAPCORE_EXPORT LocalDataProvider : public DataProvider\n{\npublic:\n    LocalDataProvider(int dataType);\n    virtual bool Request(void* param) override;\n};\n";
    let result = extract_source("provider.h", header, Some(Language::Cpp));
    let found = symbols(&result);
    assert!(
        !found.contains(&"function LocalDataProvider".to_string()),
        "{found:?}"
    );
    assert!(
        found.contains(&"class DataProvider".to_string()),
        "{found:?}"
    );

    let source = "\n#define API __declspec(dllexport)\nstruct API Widget : public Base { int x; };\nclass Plain : public Base { public: int y; };\n";
    let result = extract_source("widget.cpp", source, Some(Language::Cpp));
    let found = symbols(&result);
    assert!(!found.contains(&"function Widget".to_string()), "{found:?}");
    assert!(found.contains(&"class Plain".to_string()), "{found:?}");
}

/// The residue the export-macro recovery does not read: a lowercase macro,
/// with or without a base clause, mints no function, whichever name the
/// misparse gives it (the base's, `Base`, or the class's, `Zed`).
#[test]
fn a_lowercase_macro_before_a_class_name_mints_no_function() {
    let source = "class my_export Foo : public Base {\n public:\n  void run() {}\n};\nclass lower_api Zed {\n  int z;\n};\n";
    let result = extract_source("shapes.cpp", source, Some(Language::Cpp));
    let found = symbols(&result);
    assert!(
        !found.iter().any(|symbol| symbol.starts_with("function ")),
        "{found:?}"
    );
}

/// Genuine code keeps its functions: a real inline-defined return type has a
/// body, and an elaborated return type on a real function has a function
/// declarator.
#[test]
fn genuine_functions_returning_a_class_stay() {
    let source = "struct P { int x; } makeP() { return P{}; }\nclass Foo f() { return Foo(); }\n";
    let result = extract_source("genuine.cpp", source, Some(Language::Cpp));
    let found = symbols(&result);
    assert!(found.contains(&"function makeP".to_string()), "{found:?}");
    assert!(found.contains(&"function f".to_string()), "{found:?}");
}
