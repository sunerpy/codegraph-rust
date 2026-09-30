//! Focused C-family extraction regressions from the post-v1.6 upstream audit.

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use codegraph_extract::extract_source;

fn names_of_kind(source: &str, language: Language, kind: NodeKind) -> Vec<String> {
    let mut names = extract_source("fixture.cpp", source, Some(language))
        .nodes
        .into_iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.qualified_name)
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn c_designated_macro_arguments_do_not_swallow_following_functions() {
    let source = concat!(
        "void f(void)\n",
        "{\n",
        "    RESET_CONFIG(profile_t, profile,\n",
        "        .pid = { [PID_ROLL] = 10, [PID_YAW] = { 50, 75 } },\n",
        "        .limit = 500,\n",
        "    );\n",
        "    log_value(.5);\n",
        "    OTHER_MACRO(a == b, c);\n",
        "}\n\n",
        "void g(void) {}\n",
        "int h(void) { return 1; }\n",
    );
    let result = extract_source("pid.c", source, Some(Language::C));
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);
    let mut functions = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .collect::<Vec<_>>();
    functions.sort_by_key(|node| node.start_line);
    assert_eq!(
        functions
            .iter()
            .map(|node| node.qualified_name.as_str())
            .collect::<Vec<_>>(),
        vec!["f", "g", "h"]
    );
    assert_eq!(functions[0].end_line, 9);
    assert!(result.unresolved_references.iter().any(|reference| {
        reference.reference_kind == EdgeKind::Calls && reference.reference_name == "log_value"
    }));
    assert!(result.unresolved_references.iter().any(|reference| {
        reference.reference_kind == EdgeKind::Calls && reference.reference_name == "OTHER_MACRO"
    }));
}

#[test]
fn cpp_raw_string_macro_text_is_opaque_but_real_annotations_still_blank() {
    let source = concat!(
        "namespace {\n",
        "const char* text = u8R\"TAG(\n",
        "DECLARE_THING(\n",
        "struct Ignored { int value; };\n",
        "UE_DEPRECATED(\n",
        "class GHOST_API IgnoredApi {};\n",
        "float4 position [[position]];\n",
        "__global__ void ignored_kernel() {}\n",
        "ignored_kernel<<<1, 1>>>();\n",
        ")TAG\";\n",
        "}\n\n",
        "UPROPERTY(EditAnywhere)\n",
        "int actual_field;\n\n",
        "int create_scaffold(int x) { return x; }\n",
        "int helper_after(int y) { return y + 1; }\n",
    );
    let result = extract_source("scaffold.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);
    assert_eq!(
        names_of_kind(source, Language::Cpp, NodeKind::Function),
        vec!["create_scaffold", "helper_after"]
    );
    assert!(
        result.nodes.iter().all(|node| node.name != "Ignored"),
        "raw-string source must not become syntax: {:?}",
        result.nodes
    );
}

#[test]
fn cpp_annotation_with_raw_argument_balances_on_the_real_closer() {
    let source = concat!(
        "ANNOTATE(R\"TAG(\" ) unbalanced ( \"\n",
        "UE_DEPRECATED(\n",
        ")TAG\")\n",
        "int helper_after(void) { return 1; }\n",
    );
    let result = extract_source("annotated.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);
    assert_eq!(
        names_of_kind(source, Language::Cpp, NodeKind::Function),
        vec!["helper_after"]
    );
}

#[test]
fn cpp_pure_virtual_declarations_become_abstract_methods_only() {
    let source = concat!(
        "class Store {\n",
        "public:\n",
        "    virtual int read(int key) = 0;\n",
        "    virtual Store* clone() = 0;\n",
        "    virtual Store& operator=(const Store&) = 0;\n",
        "    int inherited_pure() = 0;\n",
        "    int not_pure(int key = 0);\n",
        "    int (*callback)(int) = 0;\n",
        "    int data = 0;\n",
        "};\n",
        "class DiskStore : public Store {\n",
        "public:\n",
        "    int read(int key) override { return key; }\n",
        "};\n",
        "int fetch(Store* store, int key) { return store->read(key); }\n",
    );
    let result = extract_source("store.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);
    let methods = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method)
        .collect::<Vec<_>>();
    for qualified in [
        "Store::read",
        "Store::clone",
        "Store::operator=",
        "Store::inherited_pure",
    ] {
        let method = methods
            .iter()
            .find(|node| node.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}; methods={methods:#?}"));
        assert!(method.is_abstract, "{qualified} must be abstract");
    }
    assert!(
        methods
            .iter()
            .any(|node| { node.qualified_name == "DiskStore::read" && !node.is_abstract })
    );
    assert!(methods.iter().all(|node| node.name != "not_pure"));
    assert!(methods.iter().all(|node| node.name != "callback"));
    assert!(methods.iter().all(|node| node.name != "data"));
    assert!(result.unresolved_references.iter().any(|reference| {
        reference.reference_kind == EdgeKind::Calls
            && matches!(reference.reference_name.as_str(), "read" | "store.read")
    }));
}

// ---- single-argument function macros (#1373) --------------------------------

fn functions(file: &str, source: &str, language: Language) -> Vec<codegraph_core::types::Node> {
    let mut functions = extract_source(file, source, Some(language))
        .nodes
        .into_iter()
        .filter(|node| node.kind == NodeKind::Function)
        .collect::<Vec<_>>();
    functions.sort_by_key(|node| (node.start_line, node.start_column));
    functions
}

#[test]
fn single_argument_function_macros_recover_their_function_name() {
    let source = "#define NATIVE_FN(name) int name(void)\n\
                  NATIVE_FN(get_version) { return helper(); }\n\
                  int use_it(void) { return get_version(); }\n";
    for (file, language) in [("main.c", Language::C), ("main.cpp", Language::Cpp)] {
        let result = extract_source(file, source, Some(language));
        let found = functions(file, source, language);
        assert_eq!(
            found
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["get_version", "use_it"],
            "{file}"
        );
        let recovered = &found[0];
        assert_eq!(
            (
                recovered.qualified_name.as_str(),
                recovered.start_line,
                recovered.end_line,
                recovered.start_column
            ),
            ("get_version", 2, 2, 0),
            "{file}"
        );
        let calls = |from: &str, name: &str| {
            result.unresolved_references.iter().any(|reference| {
                reference.from_node_id == from
                    && reference.reference_name == name
                    && reference.reference_kind == EdgeKind::Calls
            })
        };
        assert!(calls(&found[0].id, "helper"), "{file}");
        assert!(calls(&found[1].id, "get_version"), "{file}");
    }
}

#[test]
fn single_argument_function_macros_are_never_guessed() {
    let prefixes = [
        "",
        "// #define NATIVE_FN(name) int name(void)\n",
        "#define NATIVE_FN(name) int fixed(name)\n",
        "#define NATIVE_FN(name) int test_ ## name(void)\n",
        "#define NATIVE_FN(name) register_test(name)\n",
        "#define NATIVE_FN(name) typedef int name(void)\n",
        "#define NATIVE_FN(name) int name(void)\n#define NATIVE_FN int\n",
        "#define NATIVE_FN(name) int name(void)\n#ifdef OTHER\n#undef NATIVE_FN\n#endif\n",
        "#define NATIVE_FN(name) int name(void)\n#undef NATIVE_FN\n",
        "#define NATIVE_FN(name) int name(void)\n#define NATIVE_FN(name) int fixed(name)\n",
    ];
    for (file, language) in [("main.c", Language::C), ("main.cpp", Language::Cpp)] {
        for prefix in prefixes {
            let source = format!("{prefix}NATIVE_FN(candidate) {{ return 1; }}\n");
            assert!(
                !functions(file, &source, language)
                    .iter()
                    .any(|node| node.name == "candidate"),
                "{file}: {prefix:?}"
            );
        }
        let alternate = "#ifdef OTHER\n#define NATIVE_FN(name) int name(void)\n#else\n\
                         NATIVE_FN(candidate) { return 1; }\n#endif\n";
        assert!(
            !functions(file, alternate, language)
                .iter()
                .any(|node| node.name == "candidate"),
            "{file}: #else branch"
        );
        let ordinary = functions(file, "int (parenthesized)(void) { return 1; }\n", language);
        assert_eq!(
            ordinary.first().map(|node| node.name.as_str()),
            Some("(parenthesized)"),
            "{file}"
        );
    }
    let knr = functions(
        "knr.c",
        "int old_style(arg) int arg; { return arg; }\n",
        Language::C,
    );
    assert_eq!(
        knr.first().map(|node| node.name.as_str()),
        Some("old_style")
    );
}
