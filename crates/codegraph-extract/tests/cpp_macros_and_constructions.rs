//! C/C++ extraction for upstream v1.6.1 #1838 / #1839: a function-like macro is
//! a `constant` carrying its directive, so the resolver can tell a macro
//! expansion from a call; a local declaration emits one constructor reference
//! per object it constructs, and none for what constructs nothing.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;
use codegraph_extract::lang::cpp_constructor_reference_name;

fn extract(file: &str, source: &str, language: Language) -> ExtractionResult {
    let result = extract_source(file, source, Some(language));
    assert!(result.errors.is_empty(), "{file}: {:?}", result.errors);
    result
}

/// `(name, signature)` of every constant.
fn constants(result: &ExtractionResult) -> Vec<(&str, Option<&str>)> {
    result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Constant)
        .map(|node| (node.name.as_str(), node.signature.as_deref()))
        .collect()
}

/// Constructor reference arities `from` emitted for `type_name`, in order.
fn constructions(result: &ExtractionResult, from: &str, type_name: &str) -> Vec<usize> {
    let from = result
        .nodes
        .iter()
        .find(|node| node.name == from && node.kind == NodeKind::Function)
        .unwrap_or_else(|| panic!("no function {from}"));
    result
        .unresolved_references
        .iter()
        .filter(|reference| {
            reference.from_node_id == from.id && reference.reference_kind == EdgeKind::Calls
        })
        .filter_map(|reference| {
            (0..8).find(|arity| {
                reference.reference_name == cpp_constructor_reference_name(type_name, *arity)
            })
        })
        .collect()
}

#[test]
fn a_function_like_macro_is_a_constant_carrying_its_directive() {
    for (file, language) in [("trace.h", Language::C), ("trace.hpp", Language::Cpp)] {
        let result = extract(
            file,
            concat!(
                "#define TRACE_POINT(value) ((void)(value))\n",
                "#define LIMIT 10\n",
                "#define WIDE(a, b) \\\n",
                "    ((a) + (b))\n",
                "int run(void) {\n",
                "#define LOCAL_MAX(a, b) ((a) > (b) ? (a) : (b))\n",
                "    return LOCAL_MAX(1, 2);\n",
                "}\n",
            ),
            language,
        );
        assert_eq!(
            constants(&result),
            vec![
                (
                    "TRACE_POINT",
                    Some("#define TRACE_POINT(value) ((void)(value))")
                ),
                ("WIDE", Some("#define WIDE(a, b) \\\n    ((a) + (b))")),
                (
                    "LOCAL_MAX",
                    Some("#define LOCAL_MAX(a, b) ((a) > (b) ? (a) : (b))")
                ),
            ],
            "{file}: an object-like #define is a value, not a macro call target"
        );
    }
}

#[test]
fn one_constructor_reference_per_declarator_with_its_own_arity() {
    let result = extract(
        "case.cpp",
        "struct Widget { Widget() {} Widget(int value) {} };\n\
         void run() { Widget a, b(1), c{}; ns::Other d{1, 2}; Widget *p{}; }\n\
         void commented() { Widget e(/* note */ 1); }\n",
        Language::Cpp,
    );
    assert_eq!(constructions(&result, "run", "Widget"), vec![0, 1, 0]);
    assert_eq!(constructions(&result, "run", "ns::Other"), vec![2]);
    assert_eq!(constructions(&result, "commented", "Widget"), vec![1]);
    // The #1035 type-level relationship is kept for initialized declarations.
    let instantiated: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::Instantiates)
        .map(|reference| reference.reference_name.as_str())
        .collect();
    assert!(instantiated.contains(&"Widget"), "{instantiated:?}");
}

#[test]
fn pointers_references_function_declarators_and_extern_construct_nothing() {
    let result = extract(
        "controls.cpp",
        "struct Widget { Widget() {} Widget(int) {} };\n\
         void pointers() { Widget *p{}; Widget *q(nullptr); Widget *arr[2]{}; Widget (*fn)(){}; }\n\
         void reference_bind(Widget &other) { Widget &r{other}; Widget &s(other); }\n\
         void prototype() { Widget most_vexing(); extern Widget external; }\n\
         void actual() { Widget object{}; }\n",
        Language::Cpp,
    );
    for name in ["pointers", "reference_bind", "prototype"] {
        assert_eq!(
            constructions(&result, name, "Widget"),
            Vec::<usize>::new(),
            "{name}"
        );
    }
    assert_eq!(constructions(&result, "actual", "Widget"), vec![0]);
}

#[test]
fn array_braces_hold_elements_each_constructed_by_its_own_arity() {
    let result = extract(
        "arrays.cpp",
        "struct Widget { Widget() {} Widget(int value) {} };\n\
         int argument() { return 1; }\n\
         void plain() { Widget items[2]; }\n\
         void empty() { Widget items[2]{}; }\n\
         void elements() { Widget items[3]{{argument()}, {2}}; }\n\
         void grid() { Widget items[2][2]{{{1}, {2}}, {{3}}}; }\n\
         void scalar_elements() { Widget items[2]{1, 2}; }\n\
         void unknown_size(int n) { Widget items[n]{{1}}; }\n",
        Language::Cpp,
    );
    assert_eq!(constructions(&result, "plain", "Widget"), vec![0]);
    assert_eq!(constructions(&result, "empty", "Widget"), vec![0]);
    // Two braced elements, and the third default-constructed.
    assert_eq!(constructions(&result, "elements", "Widget"), vec![1, 1, 0]);
    assert_eq!(constructions(&result, "grid", "Widget"), vec![1, 1, 1, 0]);
    assert_eq!(
        constructions(&result, "scalar_elements", "Widget"),
        vec![1, 1]
    );
    // A size no literal states says nothing about the elements left over.
    assert_eq!(constructions(&result, "unknown_size", "Widget"), vec![1]);
    // A nested argument call keeps its own reference.
    let elements = result
        .nodes
        .iter()
        .find(|node| node.name == "elements")
        .unwrap();
    assert!(result.unresolved_references.iter().any(|reference| {
        reference.from_node_id == elements.id
            && reference.reference_kind == EdgeKind::Calls
            && reference.reference_name == "argument"
    }));
}

#[test]
fn an_in_class_constructor_prototype_is_a_method_carrying_its_parameters() {
    let result = extract(
        "widget.hpp",
        "class Widget {\npublic:\n    Widget();\n    explicit Widget(int value = 7);\n    int size() const;\n};\n\
         Widget::Widget() {}\n",
        Language::Cpp,
    );
    let mut methods: Vec<(&str, Option<&str>, i64)> = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method)
        .map(|node| {
            (
                node.qualified_name.as_str(),
                node.signature.as_deref(),
                node.start_line,
            )
        })
        .collect();
    methods.sort_by_key(|method| method.2);
    assert_eq!(
        methods,
        vec![
            ("Widget::Widget", Some("();"), 3),
            ("Widget::Widget", Some("(int value = 7);"), 4),
            ("Widget::Widget", Some("()"), 7),
        ],
        "only constructor prototypes become nodes; `size()` stays a declaration"
    );
}
