//! Dart `const` constructors and redirecting factories are methods
//! (upstream #2380, its kernel-parity fixture `TortureConstCtors.dart`):
//! named by the constructor, returning their class, with a parameters-only
//! signature; the unnamed ones stay unindexed, as the unnamed generative
//! constructor is. A redirecting factory references the class it constructs
//! (an UpperCamel name of its target), never the target's constructor or an
//! import prefix.

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use codegraph_extract::extract_source;

const SOURCE: &str = "import 'package:meta/meta.dart';\nimport 'package:other/other.dart' as p;\n\nconst kDepth = 3;\n\n/// A provider with const constructors and redirecting factories.\nclass ProviderT<T> {\n  /// The unnamed const constructor: no node.\n  const ProviderT({required this.create, this.lazy = true}) : _value = null;\n\n  /// A named const constructor that redirects.\n  const ProviderT.value({required T value, int depth = kDepth})\n      : this._(value: value);\n\n  @visibleForTesting\n  const ProviderT._({T? value})\n      : create = null,\n        lazy = false,\n        _value = value;\n\n  const factory ProviderT.redirect({int a}) = _ImplT<T>;\n\n  factory ProviderT.plain() = _ImplT<T>.named;\n\n  const factory ProviderT.json(Map<String, Object?> json) = _ImplT.fromJson;\n\n  const factory ProviderT.prefixed() = p.OtherT;\n\n  const factory ProviderT.prefixedNamed() = p.OtherT.named;\n\n  const factory ProviderT.unnamedTarget() = _ImplT;\n\n  const factory ProviderT() = _ImplT.named;\n\n  final Object? create;\n  final bool lazy;\n  final T? _value;\n\n  void handle(Object value) => accept(value);\n\n  static ProviderT<int> make() => ProviderT<int>.value(value: 1);\n\n  static Object typed(dynamic box) {\n    ProviderT<int>.value(value: 2);\n    final made = ProviderT<int>.value(value: 3);\n    accept(ProviderT<int>.value(value: 4));\n    wrap(child: ProviderT<int>.value(value: 5));\n    box.inner.value(6);\n    return p.OtherT<int>.named(made);\n  }\n}\n\nclass _ImplT<T> extends ProviderT<T> {\n  const _ImplT({int a = 0}) : super._();\n\n  const _ImplT.named() : super._();\n\n  const _ImplT.fromJson(Map<String, Object?> json) : super._();\n}\n\nclass _$GeneratedT {\n  const _$GeneratedT.create();\n}\n\nenum StatusT {\n  ok(200),\n  gone.named(410);\n\n  const StatusT(this.code);\n\n  const StatusT.named(this.code);\n\n  final int code;\n}\n\nextension type const MetersT(int value) {\n  const MetersT.zero() : this(0);\n}\n\nfinal topProvider = ProviderT<int>.value(value: 7);\n\nObject build() => ProviderT.value(value: 8);\n\nvoid accept(Object o) {}\n\nvoid wrap({required Object child}) {}\n";

#[test]
fn named_const_constructors_and_redirecting_factories_are_methods() {
    let result = extract_source("lib/torture.dart", SOURCE, Some(Language::Dart));
    let methods: Vec<(String, Option<String>, Option<String>)> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| {
            (
                n.qualified_name.clone(),
                n.signature.clone(),
                n.return_type.clone(),
            )
        })
        .collect();
    let method = |q: &str, sig: &str, ret: Option<&str>| {
        (
            q.to_string(),
            Some(sig.to_string()),
            ret.map(str::to_string),
        )
    };
    assert_eq!(
        methods,
        [
            method(
                "ProviderT::value",
                "({required T value, int depth = kDepth})",
                Some("ProviderT")
            ),
            method("ProviderT::_", "({T? value})", Some("ProviderT")),
            method("ProviderT::redirect", "({int a})", Some("ProviderT")),
            method("ProviderT::plain", "()", Some("ProviderT")),
            method(
                "ProviderT::json",
                "(Map<String, Object?> json)",
                Some("ProviderT")
            ),
            method("ProviderT::prefixed", "()", Some("ProviderT")),
            method("ProviderT::prefixedNamed", "()", Some("ProviderT")),
            method("ProviderT::unnamedTarget", "()", Some("ProviderT")),
            method("ProviderT::handle", "(Object value)", None),
            method("ProviderT::make", "()", Some("ProviderT")),
            method("ProviderT::typed", "(dynamic box)", Some("Object")),
            method("_ImplT::named", "()", Some("_ImplT")),
            method(
                "_ImplT::fromJson",
                "(Map<String, Object?> json)",
                Some("_ImplT")
            ),
            method("_$GeneratedT::create", "()", Some("_$GeneratedT")),
            method("StatusT::named", "(this.code)", Some("StatusT")),
            method("MetersT::zero", "()", Some("MetersT")),
        ]
    );
}

#[test]
fn a_redirecting_factory_references_the_class_it_constructs() {
    let result = extract_source("lib/torture.dart", SOURCE, Some(Language::Dart));
    let references_of = |name: &str| -> Vec<String> {
        let node = result
            .nodes
            .iter()
            .find(|n| n.qualified_name == format!("ProviderT::{name}"))
            .unwrap_or_else(|| panic!("ProviderT::{name}"));
        result
            .unresolved_references
            .iter()
            .filter(|r| r.from_node_id == node.id && r.reference_kind == EdgeKind::References)
            .map(|r| r.reference_name.clone())
            .collect()
    };
    assert_eq!(references_of("redirect"), ["_ImplT", "T"]);
    assert_eq!(references_of("plain"), ["_ImplT", "T"]);
    assert_eq!(references_of("json"), ["Map", "Object", "_ImplT"]);
    assert_eq!(references_of("prefixed"), ["OtherT"]);
    assert_eq!(references_of("prefixedNamed"), ["OtherT"]);
    assert_eq!(references_of("unnamedTarget"), ["_ImplT"]);
    // The unnamed redirecting factory (`const factory ProviderT() =
    // _ImplT.named;`) is no node, and neither its target's constructor nor
    // its class is anything's reference.
    assert!(
        !result.unresolved_references.iter().any(|r| r.line == 33
            || (r.reference_name == "named" && r.reference_kind == EdgeKind::References)),
        "{:?}",
        result.unresolved_references
    );
}

/// KEEP-RUST: a constructor called with type arguments,
/// `ProviderT<int>.value(…)`, is recorded by its member where the chain
/// starts, as a chain link is; the chain reader types `ProviderT<int>` as the
/// type (W4-DART-03b), where upstream records `ProviderT.value`.
#[test]
fn a_constructor_called_with_type_arguments_is_recorded_where_its_type_is_written() {
    let result = extract_source("lib/torture.dart", SOURCE, Some(Language::Dart));
    let mut calls: Vec<(i64, i64, String)> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && r.reference_name.ends_with("value"))
        .map(|r| (r.line, r.col, r.reference_name.clone()))
        .collect();
    calls.sort();
    let at = |line: i64, col: i64, name: &str| (line, col, name.to_string());
    assert_eq!(
        calls,
        [
            at(41, 34, "value"),
            at(44, 4, "value"),
            at(45, 17, "value"),
            at(46, 11, "value"),
            at(47, 16, "value"),
            at(48, 4, "inner.value"),
            at(80, 20, "value"),
            at(82, 18, "ProviderT.value"),
        ]
    );
}

/// The names before the parameters name a constructor; a redirect's target
/// constructor after them names nothing (`factory Box() = _BoxImpl<int>.named;`
/// is the unnamed factory). One that names no enclosing type is error
/// recovery's, and no node.
#[test]
fn a_constructor_is_named_before_its_parameters_and_by_its_own_type() {
    let source = "class Box {\n  factory Box() = _BoxImpl<int>.named;\n  const factory Box.of() = _BoxImpl<int>.named;\n}\n\nclass _BoxImpl<T> implements Box {\n  const _BoxImpl.named();\n}\n\nclass A {\n  const B.made();\n}\n";
    let result = extract_source("lib/box.dart", source, Some(Language::Dart));
    let methods: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert_eq!(methods, ["Box::of", "_BoxImpl::named"]);
}
