use codegraph_core::types::{EdgeKind, Language, NodeKind};
use codegraph_extract::{detect_language, extract_source};

#[test]
fn scala_extracts_core_symbols() {
    let source = r#"
package demo
import scala.collection.mutable
trait Greeter { def greet(name: String): String }
class ConsoleGreeter extends Greeter {
  val prefix: Helper = Helper()
  def greet(name: String): String = prefix.render(name)
}
enum Color { case Red, Blue }
type Alias = Greeter
"#;
    let result = extract_source("src/Main.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Trait, "Greeter");
    assert_node(&result.nodes, NodeKind::Class, "ConsoleGreeter");
    assert_node(&result.nodes, NodeKind::Field, "prefix");
    assert_node(&result.nodes, NodeKind::Method, "greet");
    assert_node(&result.nodes, NodeKind::Enum, "Color");
    assert_node(&result.nodes, NodeKind::TypeAlias, "Alias");
}

#[test]
fn lua_extracts_functions_methods_variables_and_require() {
    let source = r#"
local http = require("net.http")
local M = {}
function M.connect(url)
  http.get(url)
end
function M:close()
  cleanup()
end
return M
"#;
    let result = extract_source("src/plugin.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "net.http");
    assert_node(&result.nodes, NodeKind::Method, "connect");
    assert_node(&result.nodes, NodeKind::Method, "close");
    assert_ref(&result.unresolved_references, EdgeKind::Imports, "net.http");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "cleanup");
}

#[test]
fn luau_uses_distinct_luau_language_and_adds_type_aliases() {
    assert_eq!(detect_language("src/service.luau"), Language::Luau);
    assert_eq!(detect_language("src/service.lua"), Language::Lua);
    let source = r#"
export type User = { name: string }
local Signal = require(script.Parent.Signal)
function Signal.new(name: string): User
  return { name = name }
end
"#;
    let result = extract_source("src/service.luau", source, Some(Language::Luau));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::TypeAlias, "User");
    assert_node(&result.nodes, NodeKind::Import, "Signal");
    assert_node(&result.nodes, NodeKind::Method, "new");
}

#[test]
fn dart_extracts_imports_classes_methods_enum_type_alias_and_calls() {
    let source = r#"
import 'package:flutter/widgets.dart';
typedef Handler = void Function();
enum Mode { fast, slow }
class WidgetFactory {
  factory WidgetFactory.create() { return WidgetFactory._(); }
  WidgetFactory._();
  Widget build() { return const Text('hi'); }
}
void boot() { runApp(WidgetFactory.create().build()); }
"#;
    let result = extract_source("lib/main.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(
        &result.nodes,
        NodeKind::Import,
        "package:flutter/widgets.dart",
    );
    assert_node(&result.nodes, NodeKind::Class, "WidgetFactory");
    assert_node(&result.nodes, NodeKind::Method, "create");
    assert_node(&result.nodes, NodeKind::Method, "build");
    assert_node(&result.nodes, NodeKind::Enum, "Mode");
    assert_node(&result.nodes, NodeKind::TypeAlias, "Handler");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "runApp");
}

#[test]
fn pascal_extracts_source_extensions_but_not_dfm() {
    assert_eq!(detect_language("app/main.pas"), Language::Pascal);
    assert_eq!(detect_language("app/main.dpr"), Language::Pascal);
    assert_eq!(detect_language("app/pkg.dpk"), Language::Pascal);
    assert_eq!(detect_language("app/prog.lpr"), Language::Pascal);
    assert_eq!(detect_language("app/form.dfm"), Language::Pascal);
    let source = r#"
unit Demo;
interface
uses SysUtils;
type
  TGreeter = class
  public
    procedure SayHello(Name: string);
  end;
implementation
procedure TGreeter.SayHello(Name: string);
begin
  WriteLn(Name);
end;
end.
"#;
    let result = extract_source("src/demo.pas", source, Some(Language::Pascal));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "TGreeter");
    assert_node(&result.nodes, NodeKind::Method, "SayHello");
    assert_node(&result.nodes, NodeKind::Import, "SysUtils");
}

#[test]
fn objc_extracts_interfaces_protocols_properties_methods_and_imports() {
    let source = r#"
#import <Foundation/Foundation.h>
@protocol Greeter
- (void)greet:(NSString *)name;
@end
@interface Person : NSObject <Greeter>
@property (nonatomic, strong) NSString *name;
- (void)greet:(NSString *)name;
@end
@implementation Person
- (void)greet:(NSString *)name { NSLog(@"%@", name); }
@end
"#;
    let result = extract_source("src/Person.m", source, Some(Language::ObjC));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "Foundation/Foundation.h");
    assert_node(&result.nodes, NodeKind::Protocol, "Greeter");
    assert_node(&result.nodes, NodeKind::Class, "Person");
    assert_node(&result.nodes, NodeKind::Property, "name");
    assert_node(&result.nodes, NodeKind::Method, "greet:");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "NSLog");
}

fn assert_node(nodes: &[codegraph_core::types::Node], kind: NodeKind, name: &str) {
    assert!(
        nodes
            .iter()
            .any(|node| node.kind == kind && node.name == name),
        "missing {kind:?} {name}; nodes={nodes:#?}"
    );
}

fn assert_ref(refs: &[codegraph_core::types::UnresolvedRef], kind: EdgeKind, name: &str) {
    assert!(
        refs.iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing {kind:?} {name}; refs={refs:#?}"
    );
}
