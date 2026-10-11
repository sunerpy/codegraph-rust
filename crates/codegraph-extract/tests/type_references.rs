//! A declaration's annotated types are `references` from it (G3, upstream
//! `TYPE_ANNOTATION_LANGUAGES`): parameter and return types of functions and
//! methods, and the other type positions each language's own path reads.
//! Built-in and primitive types name no project symbol and are skipped.

use codegraph_core::types::{EdgeKind, Language, NodeKind};
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

/// Java: parameter, return and field types; a qualified type names only its
/// last segment, and primitives are their own grammar kinds.
#[test]
fn java_parameter_return_and_field_types_are_references() {
    let source = "package app;\n\nclass Shop {\n    private Repo repo;\n    public Result<Out> build(Widget w, java.util.List<Item> items, int n, String s) {\n        return null;\n    }\n}\n";
    assert_eq!(
        type_refs("src/app/Shop.java", source, Language::Java),
        pairs(&[
            ("build", "Item"),
            ("build", "List"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Widget"),
            ("repo", "Repo"),
        ])
    );
}

/// Kotlin (kotlin-ng writes a type as `user_type` over identifiers):
/// parameter and return types; an extension receiver is not a parameter type.
#[test]
fn kotlin_parameter_and_return_types_are_references() {
    let source = "class Svc {\n    fun build(w: Widget, items: List<Item>, n: Int): Result<Out>? { return x }\n    fun Widget.ext(n: Int): Unit {}\n}\n";
    assert_eq!(
        type_refs("src/Svc.kt", source, Language::Kotlin),
        pairs(&[
            ("build", "Item"),
            ("build", "List"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Widget"),
        ])
    );
}

/// Scala: every parameter list of a curried definition, the return type, and
/// the bounds of its type parameters.
#[test]
fn scala_parameter_lists_return_type_and_bounds_are_references() {
    let source = "object Shop {\n  def build[A: Monoid, F <: Base](w: Widget)(implicit ord: Ordering[Item]): Result[Out] = ???\n}\n";
    assert_eq!(
        type_refs("src/Shop.scala", source, Language::Scala),
        pairs(&[
            ("build", "Base"),
            ("build", "Item"),
            ("build", "Monoid"),
            ("build", "Ordering"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Widget"),
        ])
    );
}

/// C#: only type positions are walked (a parameter's or tuple element's name
/// never surfaces), a qualified name by its last segment, and a built-in
/// (`predefined_type`) is skipped.
#[test]
fn csharp_type_positions_are_references() {
    let source = "namespace App {\n  class Svc {\n    private readonly ILogger _log;\n    public Widget Size { get; set; }\n    public Result<Out> Build(Widget w, List<Item> items, int n, (int Code, Foo Payload) t, App.Models.Order o) { return null; }\n  }\n}\n";
    assert_eq!(
        type_refs("src/Svc.cs", source, Language::CSharp),
        pairs(&[
            ("Build", "Foo"),
            ("Build", "Item"),
            ("Build", "List"),
            ("Build", "Order"),
            ("Build", "Out"),
            ("Build", "Result"),
            ("Build", "Widget"),
            ("Size", "Widget"),
            ("_log", "ILogger"),
        ])
    );
}

/// PHP: parameter and return type hints; a qualified name by its last
/// segment; primitives and pseudo-types (`self`, `null`) are skipped.
#[test]
fn php_type_hints_are_references() {
    let source = "<?php\nclass Svc {\n  public function build(?Widget $w, \\App\\Item|null $i, int $n, self $s): Result { return $x; }\n}\n";
    assert_eq!(
        type_refs("src/Svc.php", source, Language::Php),
        pairs(&[("build", "Item"), ("build", "Result"), ("build", "Widget"),])
    );
}

/// Dart: a method's signature, walked whole, since its names are
/// `identifier` and only types are `type_identifier`.
#[test]
fn dart_signature_types_are_references() {
    let source =
        "class Svc {\n  Result<Out> build(Widget w, List<Item> items, int n) { return x; }\n}\n";
    assert_eq!(
        type_refs("lib/svc.dart", source, Language::Dart),
        pairs(&[
            ("build", "Item"),
            ("build", "List"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Widget"),
        ])
    );
}

/// Swift: parameter and return types; and a protocol composition continued
/// on an `&` line inside a type's body keeps the continuation's types
/// (upstream #2108), which the grammar otherwise parses as an error.
#[test]
fn swift_signature_and_composition_types_are_references() {
    let source = "final class EditorStore {\n    typealias EditorClient = AutocompleteService.Client\n        & MediaUploadService.Client\n    func build(w: Widget, items: [Item]) -> Result<Out, Error> { fatalError() }\n}\n";
    assert_eq!(
        type_refs("Sources/EditorStore.swift", source, Language::Swift),
        pairs(&[
            ("EditorClient", "AutocompleteService"),
            ("EditorClient", "Client"),
            ("EditorClient", "Client"),
            ("EditorClient", "MediaUploadService"),
            ("build", "Error"),
            ("build", "Item"),
            ("build", "Out"),
            ("build", "Result"),
            ("build", "Widget"),
        ])
    );
}

/// W2-03d (KEEP-RUST: upstream mints these nodes without reading their
/// types): a Kotlin property's declared type is a reference from the property.
/// An extension property's receiver is not its type; a primary-constructor
/// parameter and a local mint no node, so they reference nothing.
#[test]
fn kotlin_property_types_are_references() {
    let source = "package app\n\nval shared: Repo = Repo()\n\nobject Config {\n    val clock: Clock = Clock()\n    var cache: Cache<Repo>? = null\n}\n\nclass Svc(private val ctor: Ctor) {\n    private val clock: Clock = Clock()\n    val count: Int = 0\n    val Widget.label: String get() = \"x\"\n    fun run() { val local: Local = Local() }\n}\n";
    assert_eq!(
        type_refs("src/Svc.kt", source, Language::Kotlin),
        pairs(&[
            ("cache", "Cache"),
            ("cache", "Repo"),
            ("clock", "Clock"),
            ("clock", "Clock"),
            ("shared", "Repo"),
        ])
    );
}

/// W2-03d: a PHP class property is a `field` node, one per
/// `property_element`, whose name nests as `variable_name → name` (upstream
/// `extractField`). Its signature is the declared type and `$name`, and its
/// visibility defaults to `public` as PHP's does.
#[test]
fn php_class_properties_are_field_nodes() {
    let source = "<?php\nnamespace App;\n\nclass Svc {\n    private Logger $logger;\n    public static ?Mailer $mailer = null;\n    var $untyped;\n    public $a, $b;\n}\n";
    let result = extract_source("src/Svc.php", source, Some(Language::Php));
    let mut fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Field)
        .map(|node| {
            (
                node.name.as_str(),
                node.start_line,
                node.signature.as_deref(),
                node.visibility.as_deref(),
                node.is_static,
            )
        })
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            ("a", 8, Some("$a"), Some("public"), false),
            ("b", 8, Some("$b"), Some("public"), false),
            ("logger", 5, Some("Logger $logger"), Some("private"), false),
            ("mailer", 6, Some("?Mailer $mailer"), Some("public"), true),
            ("untyped", 7, Some("$untyped"), Some("public"), false),
        ]
    );
}

/// W2-03d (KEEP-RUST: upstream's PHP field branch returns before it reads a
/// type): a PHP property's declared type is a reference from its field node,
/// a qualified name by its last segment; a primitive is skipped and an untyped
/// property references nothing.
#[test]
fn php_property_types_are_references() {
    let source = "<?php\nnamespace App;\n\nclass Svc {\n    private Logger $logger;\n    public static ?Mailer $mailer = null;\n    protected int $count = 0;\n    public Logger|Mailer $either;\n    public readonly \\App\\Models\\Order $order;\n    var $untyped;\n}\n";
    assert_eq!(
        type_refs("src/Svc.php", source, Language::Php),
        pairs(&[
            ("either", "Logger"),
            ("either", "Mailer"),
            ("logger", "Logger"),
            ("mailer", "Mailer"),
            ("order", "Order"),
        ])
    );
}
