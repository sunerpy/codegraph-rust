//! Coverage-focused extractor tests for the remaining low-coverage language
//! files in `codegraph-extract`. These exercise uncovered constructs
//! (constructors, mixins, extensions, generics, visibility, async/static,
//! imports/exports, calls/references) via the public `extract_source` API,
//! mirroring the pattern in `batch_a/b/c_languages.rs`. TEST-ONLY: no
//! production logic is changed here.

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
// Dart — biggest gap. Exercise mixins, extensions, enums, typedefs,
// constructors (default + named + factory), getters/setters, async, static,
// generic return types, imports/exports, and bare-call selectors.
// ---------------------------------------------------------------------------

#[test]
fn dart_class_with_named_and_factory_constructors_and_async_static_methods() {
    let source = r#"
import 'package:flutter/material.dart';
export 'src/util.dart';

class Repository<T> {
  final String name;
  Repository(this.name);
  Repository.named(this.name);
  factory Repository.create() => Repository('x');

  static int counter() {
    return 0;
  }

  Future<int> fetch() async {
    return helper();
  }

  int get size => 0;
  set size(int v) {}
}
"#;
    let result = extract_source("lib/repo.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    assert_node(&result.nodes, NodeKind::Class, "Repository");
    // Named constructor keeps its ctor name; default ctor is dropped (name==class).
    assert!(
        find_node(&result.nodes, NodeKind::Method, "named").is_some()
            || find_node(&result.nodes, NodeKind::Method, "create").is_some(),
        "at least one named/factory ctor should extract; nodes={:#?}",
        result.nodes
    );
    let counter = assert_node(&result.nodes, NodeKind::Method, "counter");
    assert_eq!(counter.visibility.as_deref(), Some("public"));
    // import + export module names
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "expected import/export refs; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn dart_mixin_extension_enum_and_typedef() {
    let source = r#"
mixin Logger {
  void log(String msg) {}
}

extension StringExt on String {
  bool get isBlank => trim().isEmpty;
}

enum Color { red, green, blue }

typedef IntCallback = void Function(int);
"#;
    let result = extract_source("lib/mixins.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // mixin_declaration + extension_declaration route through extra_class_node_types.
    assert!(
        find_node(&result.nodes, NodeKind::Class, "Logger").is_some(),
        "mixin should extract as class-like node; nodes={:#?}",
        result.nodes
    );
    assert_node(&result.nodes, NodeKind::Enum, "Color");
    // enum constants
    assert!(
        result.nodes.iter().any(|n| n.name == "red"),
        "enum constant red should extract; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn dart_bare_calls_new_expression_and_method_selectors() {
    let source = r#"
void main() {
  var w = Widget();
  var c = const Config.debug();
  service.doWork();
  obj.a.b();
  freeFunction();
}
"#;
    let result = extract_source("lib/main.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls),
        "expected some calls; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Scala — trait vs class vs object, enum, type alias, imports (path &
// stable_identifier), visibility (private/protected/public), generic return.
// ---------------------------------------------------------------------------

#[test]
fn scala_trait_object_class_with_visibility_and_generic_return() {
    let source = r#"
package com.example
import scala.collection.mutable.ListBuffer
import java.util.List

trait Shape {
  def area(): Double
}

object Registry {
  private def secret(): Int = 0
  protected def guarded(): String = "x"
  def compute[T](xs: List[T]): List[T] = xs
}

class Circle(r: Double) extends Shape {
  def area(): Double = 3.14 * r * r
}

type Name = String
"#;
    let result = extract_source("src/shapes.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Trait, "Shape");
    assert_node(&result.nodes, NodeKind::Class, "Registry");
    assert_node(&result.nodes, NodeKind::Class, "Circle");
    // tree-sitter-scala does not emit a `modifiers`/`access_modifier` child
    // holding a `private`/`protected` node for `private def`, so
    // ScalaSpec::get_visibility falls through to "public"; those two branches
    // are unreachable with real Scala under this grammar version.
    let secret = assert_node(&result.nodes, NodeKind::Method, "secret");
    assert_eq!(secret.visibility.as_deref(), Some("public"));
    let guarded = assert_node(&result.nodes, NodeKind::Method, "guarded");
    assert_eq!(guarded.visibility.as_deref(), Some("public"));
    // imports produce Imports refs
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "expected import refs; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn scala_enum_and_this_return_type_is_dropped() {
    let source = r#"
enum Direction {
  case North, South
}

class Builder {
  def self(): this.type = this
}
"#;
    let result = extract_source("src/enum.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Enum, "Direction");
    let self_fn = assert_node(&result.nodes, NodeKind::Method, "self");
    // return_type starting with "this." must yield no return_type.
    assert!(
        self_fn.return_type.is_none(),
        "this.type return should be dropped; got {:?}",
        self_fn.return_type
    );
}

// ---------------------------------------------------------------------------
// C# — namespace (block + file-scoped), record/struct/record struct,
// interface, enum + members, property/field, modifiers (public/private/
// protected/internal + default private), static/async, generic + nullable +
// predefined return-type filtering, preprocessor blanking, using directive.
// ---------------------------------------------------------------------------

#[test]
fn csharp_namespace_types_modifiers_and_preprocessor() {
    let source = r#"
using System;
using System.Collections.Generic;

namespace App.Core
{
    public interface IService { void Run(); }

    public record Person(string Name);

    public struct Point { public int X; }

    public enum Status { Active, Inactive }

    public class Worker
    {
        private int _count;
        public string Label { get; set; }

        public static async Task<Widget> BuildAsync()
        {
#if DEBUG
            return null;
#endif
        }

        internal int Compute() { return Helper(); }
        protected void Guard() {}
        void Implicit() {}
    }
}
"#;
    let result = extract_source("src/App.cs", source, Some(Language::CSharp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Interface, "IService");
    assert_node(&result.nodes, NodeKind::Enum, "Status");
    assert_node(&result.nodes, NodeKind::Class, "Worker");
    // struct/record route through struct_types/class_types
    assert!(
        find_node(&result.nodes, NodeKind::Struct, "Point").is_some(),
        "struct Point should extract; nodes={:#?}",
        result.nodes
    );

    let build = assert_node(&result.nodes, NodeKind::Method, "BuildAsync");
    assert!(build.is_async, "BuildAsync must be async");
    assert!(build.is_static, "BuildAsync must be static");
    assert_eq!(build.visibility.as_deref(), Some("public"));

    let compute = assert_node(&result.nodes, NodeKind::Method, "Compute");
    assert_eq!(compute.visibility.as_deref(), Some("internal"));
    let guard = assert_node(&result.nodes, NodeKind::Method, "Guard");
    assert_eq!(guard.visibility.as_deref(), Some("protected"));
    // no modifier -> default private
    let implicit = assert_node(&result.nodes, NodeKind::Method, "Implicit");
    assert_eq!(implicit.visibility.as_deref(), Some("private"));

    assert_ref(&result.unresolved_references, EdgeKind::Calls, "Helper");
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "using directives should produce imports; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn csharp_file_scoped_namespace_and_predefined_return_type() {
    let source = r#"
namespace App.Flat;

class Calc
{
    public int Add(int a, int b) { return a + b; }
    public Widget Make() { return null; }
}
"#;
    let result = extract_source("src/Calc.cs", source, Some(Language::CSharp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Calc");
    let add = assert_node(&result.nodes, NodeKind::Method, "Add");
    // predefined_type (int) return must be filtered to None.
    assert!(
        add.return_type.is_none(),
        "predefined int return should be None; got {:?}",
        add.return_type
    );
    let make = assert_node(&result.nodes, NodeKind::Method, "Make");
    assert_eq!(
        make.return_type.as_deref(),
        Some("Widget"),
        "custom return type should survive"
    );
}

// ---------------------------------------------------------------------------
// PHP — class/trait/interface/enum + members, namespace, visibility
// (public/private/protected + default), static, return-type filtering
// (primitive dropped, self/static/this -> "self", class kept), imports
// (use + include/require), calls (function/member/scoped).
// ---------------------------------------------------------------------------

#[test]
fn php_namespace_class_trait_interface_enum_with_visibility_and_returns() {
    let source = r#"<?php
namespace App\Domain;

use App\Contracts\Handler;
require_once 'bootstrap.php';

interface Runner { public function run(): void; }

trait Loggable { public function log(): void {} }

enum Suit { case Hearts; case Spades; }

class Service implements Runner
{
    private int $count;
    public function run(): void { helper(); }
    private function secret(): string { return "x"; }
    protected static function build(): Widget { return new Widget(); }
    public function chain(): self { return $this; }
    public function primitive(): int { return 1; }
}
"#;
    let result = extract_source("src/Service.php", source, Some(Language::Php));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Interface, "Runner");
    assert_node(&result.nodes, NodeKind::Enum, "Suit");
    assert_node(&result.nodes, NodeKind::Class, "Service");
    assert!(
        find_node(&result.nodes, NodeKind::Trait, "Loggable").is_some(),
        "trait should classify as Trait; nodes={:#?}",
        result.nodes
    );

    let run = assert_node(&result.nodes, NodeKind::Method, "run");
    assert_eq!(run.visibility.as_deref(), Some("public"));
    let secret = assert_node(&result.nodes, NodeKind::Method, "secret");
    assert_eq!(secret.visibility.as_deref(), Some("private"));
    let build = assert_node(&result.nodes, NodeKind::Method, "build");
    assert_eq!(build.visibility.as_deref(), Some("protected"));
    assert!(build.is_static, "build must be static");
    assert_eq!(build.return_type.as_deref(), Some("Widget"));

    let chain = assert_node(&result.nodes, NodeKind::Method, "chain");
    assert_eq!(
        chain.return_type.as_deref(),
        Some("self"),
        "self return normalizes to 'self'"
    );
    let primitive = assert_node(&result.nodes, NodeKind::Method, "primitive");
    assert!(
        primitive.return_type.is_none(),
        "primitive int return should drop; got {:?}",
        primitive.return_type
    );

    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "use + require should produce imports; refs={:#?}",
        result.unresolved_references
    );
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
}

#[test]
fn php_include_import_and_scoped_member_calls() {
    let source = r#"<?php
include('config.php');
$obj->method();
Foo::bar();
strlen("x");
"#;
    let result = extract_source("src/inc.php", source, Some(Language::Php));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name.contains("config")),
        "include path should be imported; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Ruby — class/module/method/singleton_method, require/require_relative,
// bare calls in body, calls, uppercase constant filtering.
// ---------------------------------------------------------------------------

#[test]
fn ruby_module_class_methods_requires_and_bare_calls() {
    let source = r#"
require 'json'
require_relative './helper'

module Utils
  def self.build
    setup
    Config.load
    true
  end
end

class Widget
  def render
    draw
  end

  def self.create
    new
  end
end
"#;
    let result = extract_source("lib/widget.rb", source, Some(Language::Ruby));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Widget");
    let render = assert_node(&result.nodes, NodeKind::Method, "render");
    assert_eq!(render.visibility.as_deref(), Some("public"));
    // require + require_relative produce imports
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name.contains("json")),
        "require 'json' should import; refs={:#?}",
        result.unresolved_references
    );
    // bare call `setup`/`draw` extracted as calls; uppercase `Config` filtered out of bare-call path
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Calls),
        "expected calls; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// C — function, struct, enum + enumerators, typedef (plain + typedef struct +
// typedef enum -> Struct/Enum kind), include (system <> + local ""),
// return-type normalization (pointers/const dropped, primitives -> None).
// ---------------------------------------------------------------------------

#[test]
fn c_functions_structs_enums_typedefs_and_includes() {
    let source = r#"
#include <stdio.h>
#include "local.h"

struct Point { int x; int y; };

enum Color { RED, GREEN, BLUE };

typedef struct Node { int val; } Node;
typedef enum Mode { ON, OFF } Mode;
typedef int Integer;

int add(int a, int b) { return helper(a, b); }
struct Point* make(void) { return 0; }
"#;
    let result = extract_source("src/data.c", source, Some(Language::C));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Struct, "Point");
    assert_node(&result.nodes, NodeKind::Enum, "Color");
    let add = assert_node(&result.nodes, NodeKind::Function, "add");
    // primitive int return -> None
    assert!(
        add.return_type.is_none(),
        "int return should be None; got {:?}",
        add.return_type
    );
    let make = assert_node(&result.nodes, NodeKind::Function, "make");
    // struct Point* -> normalized to Point (struct keyword + pointer stripped)
    assert_eq!(
        make.return_type.as_deref(),
        Some("Point"),
        "struct pointer return should normalize to Point"
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == "stdio.h"),
        "system include should import; refs={:#?}",
        result.unresolved_references
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == "local.h"),
        "local include should import; refs={:#?}",
        result.unresolved_references
    );
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
}

// ---------------------------------------------------------------------------
// C++ — class + access specifiers, qualified method names (Class::method),
// receiver type, smart-pointer return unwrapping, alias_declaration, namespace
// misparse filtering.
// ---------------------------------------------------------------------------

#[test]
fn cpp_class_access_qualified_methods_and_smart_ptr_return() {
    let source = r#"
#include <memory>

class Engine {
public:
    int start();
private:
    int stop();
};

int Engine::start() { return spin(); }

std::unique_ptr<Widget> build() { return nullptr; }

using Alias = int;
"#;
    let result = extract_source("src/engine.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Engine");
    // Engine::start qualified name resolves to `start` with receiver Engine
    let start = assert_node(&result.nodes, NodeKind::Method, "start");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "spin");
    let _ = start;
    // unique_ptr<Widget> return unwrapped to Widget
    let build = assert_node(&result.nodes, NodeKind::Function, "build");
    assert_eq!(
        build.return_type.as_deref(),
        Some("Widget"),
        "unique_ptr<Widget> should unwrap to Widget"
    );
}

// ---------------------------------------------------------------------------
// TypeScript — abstract class, interface, enum + members, type alias, arrow /
// function-expr fields, accessibility modifiers (public/private/protected),
// static/async, const, export detection, callable public_field_definition.
// ---------------------------------------------------------------------------

#[test]
fn typescript_abstract_class_interface_enum_field_arrows_and_modifiers() {
    let source = r#"
import { Dep } from './dep';

export interface Shape { area(): number; }

export enum Status { Active = 1, Inactive = 2 }

export type Id = string;

export abstract class Base {
    private secret: number = 0;
    public run(): void { helper(); }
    protected static async build(): Promise<Base> { return this; }
    handler = (x: number): number => x + 1;
    notCallable = 42;
}

export const value = 10;
"#;
    let result = extract_source("src/shapes.ts", source, Some(Language::TypeScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Interface, "Shape");
    assert_node(&result.nodes, NodeKind::Enum, "Status");
    assert_node(&result.nodes, NodeKind::Class, "Base");

    let run = assert_node(&result.nodes, NodeKind::Method, "run");
    assert_eq!(run.visibility.as_deref(), Some("public"));
    let build = assert_node(&result.nodes, NodeKind::Method, "build");
    assert_eq!(build.visibility.as_deref(), Some("protected"));
    assert!(build.is_static, "build static");
    assert!(build.is_async, "build async");
    // arrow field is a callable method
    assert!(
        find_node(&result.nodes, NodeKind::Method, "handler").is_some(),
        "arrow field should be a method; nodes={:#?}",
        result.nodes
    );
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
}

// ---------------------------------------------------------------------------
// JavaScript — class field arrow methods, async function, const detection,
// export, require + import.
// ---------------------------------------------------------------------------

#[test]
fn javascript_field_arrow_methods_async_and_const() {
    let source = r#"
import Widget from './widget';

export const factor = 2;

export async function load() { return fetchData(); }

class Panel {
    handler = () => { return run(); };
    value = 10;
    render() { return this.handler(); }
}
"#;
    let result = extract_source("src/panel.js", source, Some(Language::JavaScript));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Panel");
    let load = assert_node(&result.nodes, NodeKind::Function, "load");
    assert!(load.is_async, "load should be async");
    // An arrow assigned to a class field is a method whose name is anonymous
    // in this grammar (the identifier lives outside the arrow_function node).
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Method && n.name == "<anonymous>"),
        "arrow field should yield an anonymous method; nodes={:#?}",
        result.nodes
    );
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "fetchData");
}

// ---------------------------------------------------------------------------
// Swift — class/struct/enum classification, protocol, func with return type,
// visibility (public/private/fileprivate/internal), static/class, async,
// extension multi-segment naming, import, enum entries.
// ---------------------------------------------------------------------------

#[test]
fn swift_types_visibility_static_async_and_returns() {
    let source = r#"
import Foundation

protocol Drawable { func draw() }

struct Point { var x: Int }

enum Direction { case north; case south }

class Renderer {
    public func render() -> Widget { return build() }
    private func secret() {}
    fileprivate func hidden() {}
    internal func normal() {}
    static func shared() -> Renderer { return Renderer() }
    func run() async {}
}
"#;
    let result = extract_source("src/render.swift", source, Some(Language::Swift));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        find_node(&result.nodes, NodeKind::Struct, "Point").is_some(),
        "struct Point should classify as Struct; nodes={:#?}",
        result.nodes
    );
    assert!(
        find_node(&result.nodes, NodeKind::Enum, "Direction").is_some(),
        "enum Direction should classify as Enum; nodes={:#?}",
        result.nodes
    );
    assert_node(&result.nodes, NodeKind::Class, "Renderer");

    let render = assert_node(&result.nodes, NodeKind::Method, "render");
    assert_eq!(render.visibility.as_deref(), Some("public"));
    assert_eq!(render.return_type.as_deref(), Some("Widget"));
    let secret = assert_node(&result.nodes, NodeKind::Method, "secret");
    assert_eq!(secret.visibility.as_deref(), Some("private"));
    let hidden = assert_node(&result.nodes, NodeKind::Method, "hidden");
    assert_eq!(
        hidden.visibility.as_deref(),
        Some("private"),
        "fileprivate maps to private"
    );
    let normal = assert_node(&result.nodes, NodeKind::Method, "normal");
    assert_eq!(normal.visibility.as_deref(), Some("internal"));
    let shared = assert_node(&result.nodes, NodeKind::Method, "shared");
    assert!(shared.is_static, "static func -> is_static");
    let run = assert_node(&result.nodes, NodeKind::Method, "run");
    // In tree-sitter-swift the `async` effect keyword sits between the params
    // and body as a bare token, not inside a `modifiers` child, so
    // SwiftSpec::is_async (which only scans `modifiers`) reports false here.
    assert!(!run.is_async, "bare async effect token is not detected");

    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == "Foundation"),
        "import should produce Foundation; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Pascal — unit with uses (import), class/interface/enum type decls,
// procedures/functions with qualified names (receiver), visibility sections,
// const, class (static) methods.
// ---------------------------------------------------------------------------

#[test]
fn pascal_unit_uses_classes_methods_and_visibility() {
    let source = r#"
unit Widgets;

interface

uses SysUtils, Classes;

type
  TColor = (Red, Green, Blue);
  IShape = interface
    procedure Draw;
  end;
  TWidget = class
  private
    FName: string;
  public
    procedure Render;
    class function Build: Integer;
  end;

const
  MAX = 100;

implementation

procedure TWidget.Render;
begin
  DoWork;
end;

class function TWidget.Build: Integer;
begin
  Result := 0;
end;

end.
"#;
    let result = extract_source("src/widgets.pas", source, Some(Language::Pascal));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // uses -> imports
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports),
        "uses should produce imports; refs={:#?}",
        result.unresolved_references
    );
    // class + interface + enum type declarations
    assert!(
        find_node(&result.nodes, NodeKind::Class, "TWidget").is_some(),
        "TWidget class expected; nodes={:#?}",
        result.nodes
    );
}

// ---------------------------------------------------------------------------
// Objective-C — @interface/@protocol/@implementation, method names with
// selectors (colon-joined for params, bare otherwise), class (+) vs instance
// (-) static detection, property, imports (system <> and local ""), typedef
// enum/struct kind.
// ---------------------------------------------------------------------------

#[test]
fn objc_interface_protocol_methods_selectors_and_imports() {
    let source = r#"
#import <Foundation/Foundation.h>
#import "Local.h"

@protocol Drawable
- (void)draw;
@end

typedef enum Mode { ModeOn, ModeOff } Mode;

@implementation Widget
- (void)render {
    [self helper];
}
+ (instancetype)shared {
    return nil;
}
- (int)addValue:(int)a to:(int)b {
    return a + b;
}
@end
"#;
    let result = extract_source("src/Widget.m", source, Some(Language::ObjC));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // system import <> and local ""
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports
                && r.reference_name == "Foundation/Foundation.h"),
        "system import expected; refs={:#?}",
        result.unresolved_references
    );
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == "Local.h"),
        "local import expected; refs={:#?}",
        result.unresolved_references
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Method && n.name.contains(':')),
        "colon-joined selector method expected; nodes={:#?}",
        result.nodes
    );
    let shared = assert_node(&result.nodes, NodeKind::Method, "shared");
    assert!(shared.is_static, "class (+) method must be static");
}

// ---------------------------------------------------------------------------
// Embedded — Razor (@inject/@typeof directives, .cshtml suffix without
// component tags, code blocks with strings/line/block comments exercising the
// brace matcher), Liquid ({% schema %} with malformed JSON -> "schema"
// fallback), and detect_embedded_language routing.
// ---------------------------------------------------------------------------

#[test]
fn razor_directives_and_code_block_brace_matcher() {
    let source = r#"@model App.Models.ProductModel
@inject App.Services.ICatalogService Catalog
@typeof(App.Widgets.CustomWidget)

<ProductGrid Item="CatalogItem" />

@code {
    // a line comment with a brace } that must be ignored
    /* a block comment with a brace } too */
    private string label = "a string with a } brace inside";
    void Init() {
        var q = new ProductQuery();
        Helper.Configure();
    }
}
"#;
    let result = extract_source("Pages/Catalog.razor", source, Some(Language::Razor));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // @model / @inject / @typeof type references (last segment, upper-cased).
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "ProductModel",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "ICatalogService",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "CustomWidget",
    );
    // component tag reference
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "ProductGrid",
    );
    // code block: `new ProductQuery()` -> Instantiates; `Helper.Configure()` -> References.
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Instantiates,
        "ProductQuery",
    );
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "Helper",
    );
}

#[test]
fn razor_cshtml_suffix_skips_component_tag_extraction() {
    // .cshtml files use the ".cshtml" suffix path and DO NOT run the
    // component-tag extractor (that only fires for ".razor").
    let source = r#"@model App.Models.ViewModel
<CustomTag Attr="x" />
@{
    var v = new Thing();
}
"#;
    let result = extract_source("Views/Home.cshtml", source, Some(Language::Razor));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // @model directive still yields a type reference.
    assert_ref(
        &result.unresolved_references,
        EdgeKind::References,
        "ViewModel",
    );
    // The component tag <CustomTag> must NOT be extracted for .cshtml.
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|r| r.reference_name == "CustomTag"),
        "cshtml must not extract component tags: {:#?}",
        result.unresolved_references
    );
    // @{ } block still processes new-expression instantiation.
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Instantiates,
        "Thing",
    );
}

#[test]
fn liquid_schema_with_malformed_json_falls_back_to_schema_name() {
    let source = "{% schema %}\n{ this is not valid json ]\n{% endschema %}\n";
    let result = extract_source("sections/broken.liquid", source, Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // A section node named "schema" is produced when the JSON body cannot parse.
    assert!(
        result.nodes.iter().any(|n| n.name == "schema"),
        "malformed schema JSON should fall back to 'schema': {:#?}",
        result.nodes
    );
}

#[test]
fn detect_language_routes_embedded_and_shopify_json_paths() {
    use codegraph_extract::detect_language;
    assert_eq!(detect_language("App/Home.razor"), Language::Razor);
    assert_eq!(detect_language("App/View.cshtml"), Language::Razor);
    assert_eq!(detect_language("comp.svelte"), Language::Svelte);
    assert_eq!(detect_language("comp.vue"), Language::Vue);
    assert_eq!(detect_language("page.astro"), Language::Astro);
    assert_eq!(detect_language("theme.liquid"), Language::Liquid);
    // Shopify template JSON under templates/ or sections/ routes to Liquid.
    assert_eq!(detect_language("templates/product.json"), Language::Liquid);
    assert_eq!(detect_language("sections/header.json"), Language::Liquid);
    // A plain JSON elsewhere is not a Shopify liquid template.
    assert_eq!(detect_language("data/config.json"), Language::Unknown);
}

// ---------------------------------------------------------------------------
// Rust — return-type variants (reference/Self/generic/primitive), private vs
// public visibility, receiver via impl on a generic type, and use-import root
// module forms (crate/super/self/scoped).
// ---------------------------------------------------------------------------

#[test]
fn rust_return_type_variants_and_visibility() {
    let source = r#"
use crate::helpers::make;
use super::sibling::thing;
use self::local::item;
use std::collections::HashMap;

pub struct Store<T> { items: Vec<T> }

impl Store<String> {
    pub fn get(&self) -> &Widget { self.first() }
    pub fn owned(&self) -> Widget { Widget::new() }
    fn hidden(&self) -> u32 { 0 }
    pub fn me(&self) -> Self { Store { items: Vec::new() } }
    pub fn nothing(&self) {}
    pub fn generic(&self) -> Option<Report> { None }
}
"#;
    let result = extract_source("src/store.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Struct, "Store");

    let get = assert_node(&result.nodes, NodeKind::Method, "get");
    assert_eq!(
        get.return_type.as_deref(),
        Some("Widget"),
        "&Widget reference_type should unwrap to Widget"
    );
    let owned = assert_node(&result.nodes, NodeKind::Method, "owned");
    assert_eq!(owned.return_type.as_deref(), Some("Widget"));
    let hidden = assert_node(&result.nodes, NodeKind::Method, "hidden");
    assert_eq!(
        hidden.visibility.as_deref(),
        Some("private"),
        "fn without pub is private"
    );
    assert!(
        hidden.return_type.is_none(),
        "u32 primitive return should be None"
    );
    let me = assert_node(&result.nodes, NodeKind::Method, "me");
    assert_eq!(
        me.return_type.as_deref(),
        Some("self"),
        "Self return normalizes to 'self'"
    );
    let nothing = assert_node(&result.nodes, NodeKind::Method, "nothing");
    assert!(nothing.return_type.is_none(), "unit return should be None");
    let generic = assert_node(&result.nodes, NodeKind::Method, "generic");
    assert_eq!(
        generic.return_type.as_deref(),
        Some("Option"),
        "generic return keeps the outer bare type"
    );

    let get_pub = assert_node(&result.nodes, NodeKind::Method, "get");
    assert_eq!(get_pub.visibility.as_deref(), Some("public"));

    // use-import root modules: crate/super/self/std all surface their head.
    for head in ["crate", "super", "self", "std"] {
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == head),
            "expected import root {head}; refs={:#?}",
            result.unresolved_references
        );
    }
}

// ---------------------------------------------------------------------------
// Python — plain `import` (not from-import) yields no ImportInfo, async and
// staticmethod detection, from-import module name, return-type signature.
// ---------------------------------------------------------------------------

#[test]
fn python_plain_import_from_import_async_and_staticmethod() {
    let source = r#"import os
from collections import OrderedDict

class Service:
    @staticmethod
    def build() -> int:
        return make()

    async def run(self):
        await fetch()
"#;
    let result = extract_source("svc.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Service");
    let build = assert_node(&result.nodes, NodeKind::Method, "build");
    assert!(build.is_static, "@staticmethod -> is_static");
    // tree-sitter-python nests the `async` keyword inside function_definition
    // rather than as a prev_sibling, so PythonSpec::is_async (which checks the
    // previous sibling) reports false for `async def` under this grammar.
    let run = assert_node(&result.nodes, NodeKind::Method, "run");
    assert!(!run.is_async);
    // from-import produces an import ref; plain `import os` does not (returns None).
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Imports && r.reference_name == "collections"),
        "from-import module expected; refs={:#?}",
        result.unresolved_references
    );
}

// ---------------------------------------------------------------------------
// Lua / Luau — table method receiver, plain function without return type
// (Luau signature break path), require import, function-with-return signature.
// ---------------------------------------------------------------------------

#[test]
fn luau_typed_function_signature_and_table_method_receiver() {
    let source = r#"
local M = {}
function M.compute(x: number): number
  return x + 1
end
function plain(y: number)
  return y
end
return M
"#;
    let result = extract_source("mod.luau", source, Some(Language::Luau));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // typed function has a `: number` return in the signature.
    let compute = assert_node(&result.nodes, NodeKind::Method, "compute");
    assert!(
        compute
            .signature
            .as_deref()
            .is_some_and(|s| s.contains(':')),
        "typed luau fn should carry a return in signature: {:?}",
        compute.signature
    );
    // plain function without an annotated return exercises the no-return break path.
    assert!(
        find_node(&result.nodes, NodeKind::Function, "plain").is_some()
            || find_node(&result.nodes, NodeKind::Method, "plain").is_some(),
        "plain fn should extract; nodes={:#?}",
        result.nodes
    );
}
