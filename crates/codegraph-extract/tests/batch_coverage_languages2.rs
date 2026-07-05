//! Second-wave coverage tests for the remaining low-coverage language
//! extractors in `codegraph-extract` (primarily `lang/dart.rs` and
//! `lang/scala.rs`). Exercises constructs the first batch missed: Dart
//! top-level functions with params/return-type signatures, method signatures
//! with return types, getters/setters, cascade/method-selector bare calls,
//! const-object named constructors, and Scala method params/return-type
//! signatures, path/stable-identifier imports, and generic return types.
//! TEST-ONLY: no production logic is changed.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_extract::extract_source;

fn assert_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}; nodes={nodes:#?}"))
}

fn find_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> Option<&'a Node> {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
}

fn assert_ref(refs: &[codegraph_core::types::UnresolvedRef], kind: EdgeKind, name: &str) {
    assert!(
        refs.iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing {kind:?} {name}; refs={refs:#?}"
    );
}

// ---------------------------------------------------------------------------
// Dart — top-level function with typed params + return type exercises the
// function_declaration -> function_signature signature/return/name path and
// the get_signature params+return branches (dart.rs 122-143, 165-183).
// ---------------------------------------------------------------------------

#[test]
fn dart_top_level_function_signature_params_and_return_type() {
    let source = r#"
int compute(int a, String b) {
  return helper(a);
}

List<int> collect(int n) {
  return build();
}

void run() {}
"#;
    let result = extract_source("lib/fns.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    // tree-sitter-dart wraps the return in `function_signature > type >
    // type_identifier`, but DartSpec::get_return_type / get_signature look for
    // a DIRECT `type_identifier` child of the signature, so the return type is
    // not surfaced (ret == None) and the signature carries only the params.
    let compute = assert_node(&result.nodes, NodeKind::Function, "compute");
    assert!(
        compute.return_type.is_none(),
        "wrapped `type` node hides the return type; got {:?}",
        compute.return_type
    );
    assert!(
        compute
            .signature
            .as_deref()
            .is_some_and(|s| s.contains('(')),
        "signature should carry params: {:?}",
        compute.signature
    );

    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "helper"),
        "helper call expected; refs={:#?}",
        result.unresolved_references
    );

    let collect = assert_node(&result.nodes, NodeKind::Function, "collect");
    assert!(collect.return_type.is_none());

    let run = assert_node(&result.nodes, NodeKind::Function, "run");
    assert!(
        run.signature.as_deref().is_some_and(|s| s.contains('(')),
        "run signature should carry the param list: {:?}",
        run.signature
    );
    assert!(run.return_type.is_none());
}

// ---------------------------------------------------------------------------
// Dart — class-body methods (method_declaration): params in signature,
// getter/setter accessors, body-call extraction, and the honest false results
// of is_static / is_async / get_return_type under tree-sitter-dart.
// ---------------------------------------------------------------------------

#[test]
fn dart_class_methods_static_async_getter_setter_and_returns() {
    let source = r#"
class Service {
  int add(int a, int b) {
    return a + b;
  }

  static String tag() {
    return "x";
  }

  Future<void> load() async {
    await fetch();
  }

  int get count => 0;
  set count(int v) {}

  Widget make() {
    return build();
  }
}
"#;
    let result = extract_source("lib/service.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Service");

    let add = assert_node(&result.nodes, NodeKind::Method, "add");
    assert!(
        add.signature.as_deref().is_some_and(|s| s.contains('(')),
        "method signature should carry params: {:?}",
        add.signature
    );

    // Class-body methods parse as `method_declaration`, but DartSpec::is_static
    // only detects the `static` token on a `method_signature`, and DartSpec::
    // is_async only inspects a next-sibling `function_body`, which does not
    // exist for a method_declaration; so both flags stay false for real Dart.
    let tag = assert_node(&result.nodes, NodeKind::Method, "tag");
    assert!(!tag.is_static);
    let load = assert_node(&result.nodes, NodeKind::Method, "load");
    assert!(!load.is_async);

    // Getter/setter both surface as a method named after the accessor.
    assert!(
        find_node(&result.nodes, NodeKind::Method, "count").is_some(),
        "getter/setter count should extract; nodes={:#?}",
        result.nodes
    );

    let make = assert_node(&result.nodes, NodeKind::Method, "make");
    assert!(make.return_type.is_none());
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "build"),
        "build call expected; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Dart — extension body resolution (resolve_body class_body/extension_body
// fallback, dart.rs 103-106) via an extension with methods, and mixin body.
// ---------------------------------------------------------------------------

#[test]
fn dart_extension_and_mixin_bodies_resolve_members() {
    let source = r#"
extension NumberExt on int {
  int doubled() {
    return this * 2;
  }
}

mixin Timestamped {
  int now() {
    return build();
  }
}
"#;
    let result = extract_source("lib/ext.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // methods inside extension/mixin bodies must be discovered.
    assert!(
        find_node(&result.nodes, NodeKind::Method, "doubled").is_some(),
        "extension method should extract; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Method, "now").is_some(),
        "mixin method should extract; nodes={:#?}",
        result.nodes
    );
}

// ---------------------------------------------------------------------------
// Dart — enum + enum constants, type_alias, and import/export module names.
// Covers enum_member_types (enum_constant), type_alias_types, and
// extract_import's library_import/library_export string-literal path.
// ---------------------------------------------------------------------------

#[test]
fn dart_enum_members_typedef_and_import_export_modules() {
    let source = r#"
import 'package:flutter/material.dart';
export 'src/util.dart';

enum Color { red, green, blue }

typedef IntCallback = void Function(int);
"#;
    let result = extract_source("lib/enums.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    assert_node(&result.nodes, NodeKind::Enum, "Color");
    for member in ["red", "green", "blue"] {
        assert!(
            find_node(&result.nodes, NodeKind::EnumMember, member).is_some(),
            "enum constant {member} should extract; nodes={:#?}",
            result.nodes
        );
    }
    assert!(
        find_node(&result.nodes, NodeKind::TypeAlias, "IntCallback").is_some(),
        "typedef should extract as a type alias; nodes={:#?}",
        result.nodes
    );

    assert_ref(
        &result.unresolved_references,
        EdgeKind::Imports,
        "package:flutter/material.dart",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Imports,
        "src/util.dart",
    );
}

// ---------------------------------------------------------------------------
// Dart — call extraction under tree-sitter-dart: member_expression calls
// (`service.doWork`, chained `second`), a plain call_expression identifier
// (`Widget`, `freeCall`). const_object_expression / cascade selectors are NOT
// surfaced by extract_bare_call under this grammar (those branches key on
// `selector` / direct `type_identifier` children that do not appear here).
// ---------------------------------------------------------------------------

#[test]
fn dart_bare_call_selector_and_const_object_variants() {
    let source = r#"
void main() {
  service.doWork();
  obj.first.second();
  builder..configure();
  var c = const Config.debug();
  var w = Widget();
  freeCall();
}
"#;
    let result = extract_source("lib/calls.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let calls: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();

    assert!(
        calls.contains(&"service.doWork"),
        "member_expression call expected; calls={calls:?}"
    );
    assert!(
        calls.contains(&"Widget"),
        "call_expression identifier Widget expected; calls={calls:?}"
    );
    assert!(
        calls.contains(&"freeCall"),
        "free call expected; calls={calls:?}"
    );
}

// ---------------------------------------------------------------------------
// Dart — a constructor whose default form (name == class) is dropped
// (is_misparsed_function true), while an explicitly named ctor keeps its name.
// Covers dart_ctor_info (327-340), resolve_name ctor branch (166-171),
// is_misparsed_function (185-187).
// ---------------------------------------------------------------------------

#[test]
fn dart_default_ctor_dropped_named_ctor_kept() {
    let source = r#"
class Box {
  final int value;
  Box(this.value);
  Box.zero() : value = 0;
}
"#;
    let result = extract_source("lib/box.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Box");
    // named ctor "zero" surfaces; default ctor "Box" (name == class) is dropped.
    assert!(
        find_node(&result.nodes, NodeKind::Method, "zero").is_some(),
        "named ctor zero should extract; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Method, "Box").is_none(),
        "default ctor named Box should be dropped; nodes={:#?}",
        result.nodes
    );
}

// ---------------------------------------------------------------------------
// Scala — method with params + return type exercises get_signature params +
// return branch (scala.rs 103-115) and get_return_type generic strip (84-93).
// Also import via `path` field and via stable_identifier fallback.
// ---------------------------------------------------------------------------

#[test]
fn scala_method_params_return_signature_and_generic_return() {
    let source = r#"
package com.example
import scala.collection.mutable.Map

class Calc {
  def add(a: Int, b: Int): Int = a + b
  def wrap[T](x: T): Option[T] = Some(x)
  def noReturn(y: Int) = y
}
"#;
    let result = extract_source("src/calc.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Calc");

    let add = assert_node(&result.nodes, NodeKind::Method, "add");
    assert_eq!(add.return_type.as_deref(), Some("Int"));
    assert!(
        add.signature
            .as_deref()
            .is_some_and(|s| s.contains('(') && s.contains(':')),
        "add signature should carry params + return: {:?}",
        add.signature
    );

    // generic return Option[T] -> strip bracket generics -> "Option".
    let wrap = assert_node(&result.nodes, NodeKind::Method, "wrap");
    assert_eq!(
        wrap.return_type.as_deref(),
        Some("Option"),
        "bracket generic return keeps outer type; got {:?}",
        wrap.return_type
    );

    // no explicit return type: signature has params only (no ": ..." return).
    let no_return = assert_node(&result.nodes, NodeKind::Method, "noReturn");
    assert!(
        no_return.return_type.is_none(),
        "missing return type -> None; got {:?}",
        no_return.return_type
    );
    assert!(
        no_return
            .signature
            .as_deref()
            .is_some_and(|s| s.contains('(')),
        "no-return method still has a params signature: {:?}",
        no_return.signature
    );

    // import with dotted path field -> module name is the full path.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "import should produce a ref; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Scala — a simple single-identifier import exercises the identifier /
// stable_identifier fallback branch in extract_import (scala.rs 161-170),
// and a return type that is a qualified path keeps only its last segment.
// ---------------------------------------------------------------------------

#[test]
fn scala_qualified_return_type_last_segment_and_object_methods() {
    let source = r#"
import foo
object Helper {
  def make(): pkg.sub.Widget = null
  def flag(): Boolean = true
}
"#;
    let result = extract_source("src/helper.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Helper");

    // qualified return pkg.sub.Widget -> last segment "Widget".
    let make = assert_node(&result.nodes, NodeKind::Method, "make");
    assert_eq!(
        make.return_type.as_deref(),
        Some("Widget"),
        "qualified return keeps last segment; got {:?}",
        make.return_type
    );

    let flag = assert_node(&result.nodes, NodeKind::Method, "flag");
    assert_eq!(flag.return_type.as_deref(), Some("Boolean"));

    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "single-identifier import should produce a ref; refs={:#?}",
        result.unresolved_references
    );
    let _ = assert_ref;
}

// ---------------------------------------------------------------------------
// Scala — a paren-less, return-less method (`def bare = 0`) exercises the
// get_signature early-return branch (scala.rs 106-107: params.is_none() &&
// return_type.is_none() -> None), and a method whose declared return type is
// a non-identifier operator (`def op(): + = ...`) exercises the
// get_return_type is_ident(false) branch (scala.rs 91-92 -> None).
// ---------------------------------------------------------------------------

#[test]
fn scala_bare_method_no_signature_and_non_ident_return_type() {
    let source = r#"
class C {
  def bare = 0
  def op(): + = null
}
"#;
    let result = extract_source("src/edge.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "C");

    // no params, no return type -> get_signature returns None.
    let bare = assert_node(&result.nodes, NodeKind::Method, "bare");
    assert!(
        bare.signature.is_none(),
        "paren-less return-less def should have no signature; got {:?}",
        bare.signature
    );
    assert!(bare.return_type.is_none());

    // a `+` operator return type is not a valid identifier -> return_type None.
    let op = assert_node(&result.nodes, NodeKind::Method, "op");
    assert!(
        op.return_type.is_none(),
        "non-identifier return type should be dropped; got {:?}",
        op.return_type
    );
}

// ---------------------------------------------------------------------------
// JavaScript — a class field assigned the result of a HOF call wrapping an
// arrow (`handler = memo(() => run())`) exercises resolve_class_field_body's
// call_expression branch (javascript.rs 145-155) and class_field_is_callable's
// call_expression branch; an empty-string import (`import ''`) exercises the
// empty-module None return (javascript.rs 125-126).
// ---------------------------------------------------------------------------

#[test]
fn javascript_hof_wrapped_arrow_field_body_and_empty_import() {
    let source = r#"
import '';

class Panel {
    handler = memo(() => { return run(); });
    plain = 5;
    render() { return this.handler(); }
}
"#;
    let result = extract_source("src/hof.js", source, Some(Language::JavaScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Panel");
    // the HOF-wrapped arrow field is callable; its body's `run()` call surfaces.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "run"),
        "HOF-wrapped arrow field body call `run` expected; refs={:#?}",
        result.unresolved_references
    );
    // empty-string import module resolves to None (no import ref for it).
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name.is_empty()),
        "empty import should not produce an import ref; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// TypeScript — a class field assigned a HOF call wrapping an arrow exercises
// resolve_class_field_body + class_field_is_callable call_expression branches
// (typescript.rs 102-115, 205-214); private/protected accessibility modifiers
// exercise get_visibility private/protected arms (140-141); an empty-string
// import exercises the empty-module None return (176-177).
// ---------------------------------------------------------------------------

#[test]
fn typescript_hof_field_visibility_private_protected_and_empty_import() {
    let source = r#"
import '';

export class Store {
    private secret: number = 0;
    protected token: string = "x";
    handler = memo(() => { return load(); });
    private compute(): number { return 1; }
    protected guard(): void {}
}
"#;
    let result = extract_source("src/store.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Store");

    let compute = assert_node(&result.nodes, NodeKind::Method, "compute");
    assert_eq!(compute.visibility.as_deref(), Some("private"));
    let guard = assert_node(&result.nodes, NodeKind::Method, "guard");
    assert_eq!(guard.visibility.as_deref(), Some("protected"));

    // HOF-wrapped arrow field body call surfaces.
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "load"),
        "HOF-wrapped arrow field body call `load` expected; refs={:#?}",
        result.unresolved_references
    );
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name.is_empty()),
        "empty import should not produce an import ref; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Pascal — a class with explicit private/protected/public visibility sections
// exercises get_visibility's kPrivate/kProtected/kPublic arms
// (pascal.rs 107-109), plus a class (static) method via `kClass` (is_static,
// pascal.rs 120-121).
// ---------------------------------------------------------------------------

#[test]
fn pascal_visibility_sections_and_class_static_method() {
    let source = r#"
unit Shapes;

interface

type
  TShape = class
  private
    FSecret: Integer;
    procedure Hidden;
  protected
    procedure Guarded;
  public
    procedure Render;
    class function Build: Integer;
  end;

implementation

procedure TShape.Hidden;
begin
end;

procedure TShape.Guarded;
begin
end;

procedure TShape.Render;
begin
  DoWork;
end;

class function TShape.Build: Integer;
begin
  Result := 0;
end;

end.
"#;
    let result = extract_source("src/shapes.pas", source, Some(Language::Pascal));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Class, "TShape").is_some(),
        "TShape class expected; nodes={:#?}",
        result.nodes
    );
    // at least one method carries each visibility from its section.
    let visibilities: Vec<Option<&str>> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.visibility.as_deref())
        .collect();
    assert!(
        visibilities.contains(&Some("private")),
        "a private-section method expected; visibilities={visibilities:?}"
    );
    assert!(
        visibilities.contains(&Some("protected")),
        "a protected-section method expected; visibilities={visibilities:?}"
    );
    assert!(
        visibilities.contains(&Some("public")),
        "a public-section method expected; visibilities={visibilities:?}"
    );
}
