//! Object construction in each language's own syntax yields an `instantiates`
//! reference named by the constructed type (G2, upstream INSTANTIATION_KINDS
//! since `8eed2432`), and a Java anonymous class body becomes a class of its
//! own (upstream `34240eb2`).

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use codegraph_extract::extract_source;

/// `(from, reference)` for every `kind` reference in `source`, sorted.
fn refs_of(path: &str, source: &str, language: Language, kind: EdgeKind) -> Vec<(String, String)> {
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
        .filter(|r| r.reference_kind == kind)
        .map(|r| (name_of(&r.from_node_id), r.reference_name.clone()))
        .collect();
    refs.sort();
    refs
}

fn instantiations(path: &str, source: &str, language: Language) -> Vec<(String, String)> {
    refs_of(path, source, language, EdgeKind::Instantiates)
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn java_object_creation_names_the_constructed_class() {
    let source = "package app;\n\nclass Shop {\n    void open() {\n        Widget w = new Widget();\n        java.util.List<String> names = new java.util.ArrayList<String>();\n        Outer.Inner inner = new Outer.Inner(1);\n    }\n}\n";
    assert_eq!(
        instantiations("src/app/Shop.java", source, Language::Java),
        pairs(&[("open", "ArrayList"), ("open", "Inner"), ("open", "Widget")])
    );
}

/// A Java anonymous class body is a class named for its type and line, which
/// extends that type and owns the methods written in it; calls in the
/// constructor arguments still belong to the enclosing method.
#[test]
fn java_anonymous_class_bodies_become_classes() {
    let source = "package app;\n\nclass Shop {\n    Runnable task() {\n        return new Runnable(label()) {\n            public void run() {\n                helper();\n            }\n        };\n    }\n}\n";
    let path = "src/app/Shop.java";
    let result = extract_source(path, source, Some(Language::Java));
    let anon = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.name == "<Runnable$anon@5>")
        .expect("the anonymous class is a class node");
    let run = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Method && node.name == "run")
        .expect("its method is a method node");
    assert!(
        result
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Contains
                && edge.source == anon.id
                && edge.target == run.id),
        "the anonymous class contains `run`"
    );
    assert_eq!(
        instantiations(path, source, Language::Java),
        pairs(&[("task", "Runnable")])
    );
    assert_eq!(
        refs_of(path, source, Language::Java, EdgeKind::Extends),
        pairs(&[("<Runnable$anon@5>", "Runnable")])
    );
    assert_eq!(
        refs_of(path, source, Language::Java, EdgeKind::Calls),
        pairs(&[("run", "helper"), ("task", "label")])
    );
}

#[test]
fn csharp_object_creation_names_the_constructed_class() {
    let source = "namespace App\n{\n    class Shop\n    {\n        void Open()\n        {\n            var w = new Widget(1);\n            var list = new List<int>();\n            var nested = new Outer.Inner();\n            Widget typed = new();\n        }\n    }\n}\n";
    assert_eq!(
        instantiations("src/Shop.cs", source, Language::CSharp),
        pairs(&[("Open", "Inner"), ("Open", "List"), ("Open", "Widget")])
    );
}

#[test]
fn php_object_creation_names_the_constructed_class() {
    let source = "<?php\n\nnamespace App;\n\nfunction open()\n{\n    $w = new Widget();\n    $q = new \\App\\Queue(3);\n}\n";
    assert_eq!(
        instantiations("src/open.php", source, Language::Php),
        pairs(&[("open", "Queue"), ("open", "Widget")])
    );
}

/// Go keeps the package qualifier, which its cross-package resolver reads;
/// slice, map and array literals construct no named type.
#[test]
fn go_composite_literals_name_the_struct_type() {
    let source = "package shop\n\nfunc Open() {\n\tw := Widget{N: 1}\n\tp := pkga.Widget{}\n\tb := Box[int]{}\n\tns := []int{1, 2}\n\tm := map[string]int{}\n\t_ = &Widget{}\n}\n";
    assert_eq!(
        instantiations("shop/open.go", source, Language::Go),
        pairs(&[
            ("Open", "Box"),
            ("Open", "Widget"),
            ("Open", "Widget"),
            ("Open", "pkga.Widget"),
        ])
    );
}

#[test]
fn rust_struct_expressions_name_the_struct() {
    let source =
        "fn open() {\n    let w = Widget { n: 1 };\n    let q = shop::Queue { size: 3 };\n}\n";
    assert_eq!(
        instantiations("src/open.rs", source, Language::Rust),
        pairs(&[("open", "Queue"), ("open", "Widget")])
    );
}

#[test]
fn scala_instance_expressions_name_the_base_type() {
    let source = "object Shop {\n  def open(): Unit = {\n    val w = new Widget(1)\n    val m = new Monoid[Int] {\n      def empty: Int = 0\n    }\n  }\n}\n";
    assert_eq!(
        instantiations("src/Shop.scala", source, Language::Scala),
        pairs(&[("open", "Monoid"), ("open", "Widget")])
    );
}
