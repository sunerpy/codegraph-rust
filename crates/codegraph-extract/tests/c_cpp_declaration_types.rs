//! C/C++ extraction for upstream #2446 (`235387ec`): a class, struct, union or
//! enum defined in the type of a variable declaration is indexed.
//!
//! ```c
//! namespace n { struct Foo { int a; void f() {} } foo; }   // only n::foo
//! static struct { int argc; char **argv; } SPT;             // nothing at all
//! enum { MODE_IDLE, MODE_BUSY } current_mode;               // no enumerators
//! ```
//!
//! The type is walked like one written on its own, beside the variables (which
//! keep their nodes), and the comment above the declaration documents it. An
//! unnamed one takes the name of the first variable its declaration declares,
//! as `typedef struct { … } Name;` takes the typedef name. In a C++ file whose
//! tree has errors the type is walked in the scopes the file's braces open
//! (#2426), and not at all when the braces do not balance.
//!
//! Upstream's cases, with two differences that predate #2446: a C++ file-scope
//! declaration mints no variable here (upstream's generic fallback makes one of
//! each bare declarator), so its C++ variable lines are not asserted; and the
//! body walk extracts no struct or union (upstream's does since v0.7.6 and
//! v1.6.0), so the function-body case is an enum and a class.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

/// Each case runs with LF and with CRLF line endings.
fn sources(source: &str) -> [String; 2] {
    [source.to_string(), source.replace('\n', "\r\n")]
}

/// `<kind> <qualified name>` for every symbol, file and imports aside.
fn symbols(result: &ExtractionResult) -> Vec<String> {
    result
        .nodes
        .iter()
        .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Import))
        .map(|node| format!("{} {}", node.kind.as_str(), node.qualified_name))
        .collect()
}

/// `<parent> > <child>` for every containment edge below the file.
fn containment(result: &ExtractionResult) -> Vec<String> {
    let qualified = |id: &str| {
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| (node.kind, node.qualified_name.clone()))
    };
    result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .filter_map(|edge| {
            let (kind, source) = qualified(&edge.source)?;
            (kind != NodeKind::File).then(|| {
                format!(
                    "{source} > {}",
                    qualified(&edge.target).map_or_else(String::new, |(_, target)| target)
                )
            })
        })
        .collect()
}

#[test]
fn cpp_classes_and_structs_sit_beside_their_variables() {
    for source in sources(concat!(
        "namespace n {\n",
        "struct Foo { int a; void f() {} } foo;\n",
        "class Bar { void g() {} } bar, *pbar;\n",
        "}\n",
    )) {
        let result = extract_source("src/decl.cpp", &source, Some(Language::Cpp));
        assert_eq!(
            symbols(&result),
            [
                "struct n::Foo",
                "method n::Foo::f",
                "class n::Bar",
                "method n::Bar::g",
            ]
        );
        assert_eq!(
            containment(&result),
            ["n::Foo > n::Foo::f", "n::Bar > n::Bar::g"]
        );
    }
}

#[test]
fn c_structs_unions_and_enums_an_unnamed_one_takes_its_variables_name() {
    for source in sources(concat!(
        "struct Foo { int a; } foo;\n",
        "static struct { int argc; char **argv; } SPT;\n",
        "static const struct { const char *name; int code; } errors[] = { { \"none\", 0 } };\n",
        "struct { int x; } *first_point, second_point;\n",
        "union { int i; float f; } scratch = { 0 };\n",
        "enum { MODE_IDLE, MODE_BUSY } current_mode;\n",
        "typedef struct Bar { int c; } Bar;\n",
    )) {
        let result = extract_source("src/decl.c", &source, Some(Language::C));
        assert_eq!(
            symbols(&result),
            [
                "struct Foo",
                "struct SPT",
                "struct errors",
                "constant errors",
                "struct first_point",
                "variable first_point",
                "union scratch",
                "variable scratch",
                "enum current_mode",
                "enum_member current_mode::MODE_IDLE",
                "enum_member current_mode::MODE_BUSY",
                "struct Bar",
            ]
        );
    }
}

#[test]
fn a_type_whose_declaration_declares_no_variable_stays_unnamed() {
    // A function's return type: naming it after the function would be wrong.
    for source in sources("struct { int unused; } make_unnamed(void);\n") {
        let result = extract_source("src/proto.c", &source, Some(Language::C));
        assert_eq!(symbols(&result), ["struct <anonymous>"]);
    }
}

/// `<kind> <docstring>` of the nodes named `Registry` or `registry`.
fn registry_docs(result: &ExtractionResult) -> Vec<String> {
    result
        .nodes
        .iter()
        .filter(|node| node.name == "Registry" || node.name == "registry")
        .map(|node| {
            format!(
                "{} {}",
                node.kind.as_str(),
                node.docstring.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

#[test]
fn the_comment_above_the_declaration_documents_the_type() {
    for source in sources(concat!(
        "namespace app {\n",
        "/** The handler registry. */\n",
        "struct Registry { int size() const { return 2; } } registry;\n",
        "}\n",
    )) {
        let result = extract_source("src/registry.cc", &source, Some(Language::Cpp));
        assert_eq!(registry_docs(&result), ["struct The handler registry."]);
    }
    // In C the variable is a node too, and keeps the comment.
    for source in sources(concat!(
        "/** The handler registry. */\n",
        "static struct Registry { int size; } *registry;\n",
    )) {
        let result = extract_source("src/registry.c", &source, Some(Language::C));
        assert_eq!(
            registry_docs(&result),
            [
                "struct The handler registry.",
                "variable The handler registry."
            ]
        );
    }
}

#[test]
fn an_unnamed_type_in_a_function_body_takes_its_variables_name_too() {
    for source in sources(concat!(
        "int hits(void) {\n",
        "  static enum { MODE_IDLE, MODE_BUSY } local_mode;\n",
        "  return local_mode;\n",
        "}\n",
    )) {
        let result = extract_source("src/stats.c", &source, Some(Language::C));
        assert_eq!(
            symbols(&result),
            [
                "function hits",
                "enum hits::local_mode",
                "enum_member hits::local_mode::MODE_IDLE",
                "enum_member hits::local_mode::MODE_BUSY",
            ]
        );
    }
    for source in sources(concat!(
        "int hits() {\n",
        "  static class { public: int count; } local_stats;\n",
        "  return ++local_stats.count;\n",
        "}\n",
    )) {
        let result = extract_source("src/stats.cc", &source, Some(Language::Cpp));
        assert_eq!(
            symbols(&result),
            ["function hits", "class hits::local_stats"]
        );
    }
}

#[test]
fn a_misread_file_c_keeps_the_type_cpp_needs_balanced_braces() {
    for source in sources("static struct { int argc; } SPT;\nint broken( { return 1; }\n") {
        let c = extract_source("src/spt.c", &source, Some(Language::C));
        assert!(
            symbols(&c).contains(&"struct SPT".to_string()),
            "{:?}",
            symbols(&c)
        );
    }
    // A C++ file with errors is walked in the scopes its braces open, so the
    // class lands in its namespace. When the braces do not balance there are
    // no scopes to trust, and error recovery can close a namespace or a class
    // at the wrong `}`: the class is not walked.
    let config = "namespace n {\nstruct Config { int retries; void apply() {} } config;\n}\n";
    for source in sources(&format!("{config}int broken( {{ return 1; }}\n")) {
        let balanced = extract_source("src/config.cc", &source, Some(Language::Cpp));
        assert_eq!(
            symbols(&balanced),
            ["struct n::Config", "method n::Config::apply"]
        );
    }
    for source in sources(&format!("{config}int broken() {{ return 1;\n")) {
        let unbalanced = extract_source("src/config2.cc", &source, Some(Language::Cpp));
        assert_eq!(symbols(&unbalanced), ["function broken"]);
    }
}

#[test]
fn a_function_value_in_the_types_body_is_captured_once_by_the_type() {
    for source in sources(concat!(
        "void on_open() {}\n",
        "struct Handlers { void (*all[1])() = {&on_open}; } handlers;\n",
    )) {
        let result = extract_source("src/handlers.cc", &source, Some(Language::Cpp));
        let refs: Vec<String> = result
            .unresolved_references
            .iter()
            .filter(|reference| reference.is_function_ref)
            .map(|reference| {
                let from = result
                    .nodes
                    .iter()
                    .find(|node| node.id == reference.from_node_id)
                    .map_or("?", |node| node.name.as_str());
                format!("{from} {}", reference.reference_name)
            })
            .collect();
        assert_eq!(refs, ["Handlers on_open"]);
    }
}
