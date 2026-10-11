//! A type read only through a static member or an enum value is a dependency
//! of the code that reads it (G4, upstream `STATIC_MEMBER_LANGS`): `Enum.value`,
//! `Type.CONST`, `Foo::BAR` give a `references` ref named by the receiver, at
//! the receiver. The receiver must be a single capitalized name; a lowercase
//! receiver (`this.helper`, `obj.field`), a longer path's head, and an access
//! that is the callee of a call (`Type.method()`, already linked) give none.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// `(reader, receiver)` for every `references` ref in `source`, sorted.
fn member_reads(path: &str, source: &str, language: Language) -> Vec<(String, String)> {
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

/// Upstream's Java seed: `JsonScope.EMPTY_DOCUMENT`. A field read whose
/// result a call is made on (`Color.RED.ordinal()`) is that call's receiver,
/// which upstream counts as the callee and skips.
#[test]
fn java_static_field_reads_reference_their_type() {
    let source = "class Reader {\n  private int helper;\n  int peek() {\n    return JsonScope.EMPTY_DOCUMENT + Limits.MAX;\n  }\n  int noop() {\n    return this.helper + Color.RED.ordinal() + Util.twice(1);\n  }\n}\n";
    assert_eq!(
        member_reads("src/Reader.java", source, Language::Java),
        pairs(&[("peek", "JsonScope"), ("peek", "Limits")])
    );
}

#[test]
fn csharp_member_reads_reference_their_type() {
    let source = "class Svc {\n  int F() { return Color.Red + Limits.Max + this.count; }\n  void G() { Console.WriteLine(1); }\n}\n";
    assert_eq!(
        member_reads("src/Svc.cs", source, Language::CSharp),
        pairs(&[("F", "Color"), ("F", "Limits")])
    );
}

#[test]
fn kotlin_member_reads_reference_their_type() {
    let source = "class Device {\n    fun sdk(): Int = Build.VERSION + Color.RED.ordinal\n    fun make() = Factory.create()\n}\n";
    assert_eq!(
        member_reads("src/Device.kt", source, Language::Kotlin),
        pairs(&[("sdk", "Build"), ("sdk", "Color")])
    );
}

#[test]
fn swift_member_reads_reference_their_type() {
    let source = "func level() -> Int {\n    let a = Color.red.rawValue\n    let b = Limits.max\n    return a + b\n}\n\nfunc make() -> Int {\n    return Factory.create()\n}\n";
    assert_eq!(
        member_reads("Sources/Level.swift", source, Language::Swift),
        pairs(&[("level", "Color"), ("level", "Limits")])
    );
}

#[test]
fn scala_member_reads_reference_their_type() {
    let source = "object Levels {\n  def level(): Int = Color.Red + Limits.Max\n  def make(): Int = Factory.create(1)\n}\n";
    assert_eq!(
        member_reads("src/Levels.scala", source, Language::Scala),
        pairs(&[("level", "Color"), ("level", "Limits")])
    );
}

/// PHP: a class constant, `Foo::class` and a static property; `self::` /
/// `static::` name no type, and `Foo::make()` is a call.
#[test]
fn php_class_constant_and_static_property_reads_reference_their_class() {
    let source = "<?php\nclass Svc {\n  function level() { return Color::RED + Limits::$max + self::X + static::Y; }\n  function kind() { return Model::class; }\n  function make() { return Factory::make(); }\n}\n";
    assert_eq!(
        member_reads("src/Svc.php", source, Language::Php),
        pairs(&[("kind", "Model"), ("level", "Color"), ("level", "Limits")])
    );
}

/// Dart writes a member access as a `member_expression`; one that is a
/// call's callee, generic or not, is the call's.
#[test]
fn dart_member_reads_reference_their_type() {
    let source = "int level(context) {\n  final p = Theme.of(context);\n  final q = Provider.of<int>(context);\n  return Color.red.index + Limits.max + context.size;\n}\n";
    assert_eq!(
        member_reads("lib/level.dart", source, Language::Dart),
        pairs(&[("level", "Color"), ("level", "Limits")])
    );
}

/// Rust: a variant path read, matched, passed as a value or patterned names
/// its receiver; `Self` is the impl's type. A call's callee (`Mode::C(1)`), a
/// struct literal, a `use` tree, a longer path's prefix and a lowercase
/// receiver or member give none.
#[test]
fn rust_variant_paths_reference_their_receiver() {
    let source = "use crate::mode::{self, Mode};\n\npub fn code(flag: bool, xs: &[u8]) -> u8 {\n    let m = if flag { mode::Mode::A } else { Mode::B };\n    let _c = Mode::C(3);\n    let _d = Mode::D { x: 2 };\n    let _n = xs.iter().map(Mode::C).count();\n    let _u = util::take(3);\n    let _f = Limits::new;\n    match m {\n        Mode::A => 1,\n        Mode::C(x) => x,\n        Mode::D { x } => x,\n        _ => 0,\n    }\n}\n\nimpl Mode {\n    pub fn flip(&self) -> u8 {\n        match self {\n            Self::A => 1,\n            _ => 0,\n        }\n    }\n}\n";
    assert_eq!(
        member_reads("src/variant_use.rs", source, Language::Rust),
        pairs(&[
            ("code", "Mode"),
            ("code", "Mode"),
            ("code", "Mode"),
            ("code", "Mode"),
            ("code", "Mode"),
            ("code", "Mode"),
            ("flip", "Mode"),
        ])
    );
}

/// tree-sitter-cpp writes the scope of `Color::RED` as a `namespace_identifier`,
/// which names no type, so a C++ scoped read gives none (as upstream).
#[test]
fn a_cpp_scoped_read_names_no_type() {
    let source = "enum Color { RED };\nint f() { return Color::RED; }\n";
    assert!(member_reads("src/f.cpp", source, Language::Cpp).is_empty());
}

/// A Swift property wrapper names a type by metatype in its arguments
/// (Fluent's `@Siblings(through: Pivot.self, …)`): the enclosing type
/// depends on it, as on the wrapper and the property's declared type.
#[test]
fn a_swift_property_wrapper_argument_references_its_type() {
    let source = "final class User {\n    @Siblings(through: Pivot.self, from: \\.$user, to: \\.$tag)\n    var tags: [Tag]\n}\n";
    assert_eq!(
        member_reads("Sources/User.swift", source, Language::Swift),
        pairs(&[("User", "Pivot"), ("User", "Tag")])
    );
}
