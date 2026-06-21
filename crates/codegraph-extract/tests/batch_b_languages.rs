use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_source};

#[test]
fn java_extracts_package_interface_class_and_methods() {
    // Upstream java.ts lines 39-59 map package/interface/class/method/import node types.
    // Java interface methods have no `modifiers` node → visibility None in the upstream getVisibility (lines 68-79).
    // This test verifies dual-greet behavior: interface method (visibility None) and class method (visibility Some("public")).
    let source = r#"
package com.example.demo;
import java.util.List;
interface Greeter { String greet(String name); }
public class ConsoleGreeter implements Greeter {
  public String greet(String name) { return List.of(name).get(0); }
}
"#;
    let result = extract_source("src/ConsoleGreeter.java", source, Some(Language::Java));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Namespace, "com.example.demo");
    assert_node(&result.nodes, NodeKind::Import, "java.util.List");
    assert_node(&result.nodes, NodeKind::Interface, "Greeter");
    assert_node(&result.nodes, NodeKind::Class, "ConsoleGreeter");
    // Interface methods have no `modifiers` child, so visibility is None in both
    // the upstream (java.ts getVisibility returns undefined) and this port; the class
    // method carries `public`. Two `greet` methods exist, so assert both.
    let mut greet_visibilities: Vec<Option<&str>> = result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method && node.name == "greet")
        .map(|node| node.visibility.as_deref())
        .collect();
    greet_visibilities.sort_unstable();
    assert_eq!(greet_visibilities, vec![None, Some("public")]);
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Implements,
        "Greeter",
    );
}

#[test]
fn c_extracts_functions_structs_and_includes() {
    // Upstream c-cpp.ts lines 98-113 define C functions/structs/includes.
    let source = r#"
#include <stdio.h>
typedef struct User { int id; } User;
User make_user(void) { printf("hi"); return (User){ .id = 1 }; }
"#;
    let result = extract_source("src/user.c", source, Some(Language::C));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "stdio.h");
    assert_node(&result.nodes, NodeKind::Struct, "User");
    let make_user = assert_node(&result.nodes, NodeKind::Function, "make_user");
    assert_eq!(make_user.return_type.as_deref(), Some("User"));
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "printf");
}

#[test]
fn cpp_extracts_namespace_class_method_and_upstream_return_type() {
    // Upstream c-cpp.ts lines 67-95 normalize C++ return types to a bare class name.
    let source = r#"
#include <memory>
namespace demo {
class Widget {
public:
  Widget child() { return Widget(); }
};
}
"#;
    let result = extract_source("src/widget.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "memory");
    assert_node(&result.nodes, NodeKind::Class, "Widget");
    let child = assert_node(&result.nodes, NodeKind::Method, "child");
    assert_eq!(child.return_type.as_deref(), Some("Widget"));
    assert_eq!(child.visibility.as_deref(), Some("public"));
}

#[test]
fn csharp_extracts_namespace_class_property_and_methods() {
    // Upstream csharp.ts lines 55-90 define namespace/class/property/method extraction.
    let source = r#"
namespace Demo.App;
using System.Collections.Generic;
public class Greeter {
  public string Name { get; set; }
  public Greeter(string name) { Name = name; }
  public string Greet() { return Name.ToString(); }
}
"#;
    let result = extract_source("src/Greeter.cs", source, Some(Language::CSharp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Namespace, "Demo.App");
    assert_node(
        &result.nodes,
        NodeKind::Import,
        "System.Collections.Generic",
    );
    assert_node(&result.nodes, NodeKind::Class, "Greeter");
    assert_node(&result.nodes, NodeKind::Property, "Name");
    assert_node(&result.nodes, NodeKind::Method, "Greet");
}

#[test]
fn ruby_extracts_module_class_def_require_and_mixin() {
    // Upstream ruby.ts lines 19-76 handle modules and include/extend/prepend as implements refs.
    let source = r#"
require 'json'
module Demo
  module Friendly; end
  class Greeter
    include Friendly
    def greet(name)
      JSON.generate(name)
    end
  end
end
"#;
    let result = extract_source("lib/greeter.rb", source, Some(Language::Ruby));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "json");
    assert_node(&result.nodes, NodeKind::Module, "Demo");
    assert_node(&result.nodes, NodeKind::Module, "Friendly");
    assert_node(&result.nodes, NodeKind::Class, "Greeter");
    assert_node(&result.nodes, NodeKind::Method, "greet");
    assert_ref(
        &result.unresolved_references,
        EdgeKind::Implements,
        "Friendly",
    );
}

#[test]
fn php_extracts_namespace_use_function_and_class() {
    // Upstream php.ts lines 68-85 define PHP namespace/use/function/class extraction.
    let source = r#"<?php
namespace Demo\App;
use Vendor\Package\Tool;
function boot(): Tool { return new Tool(); }
class Greeter { public function greet(): Tool { return boot(); } }
"#;
    let result = extract_source("src/Greeter.php", source, Some(Language::Php));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Namespace, r"Demo\App");
    assert_node(&result.nodes, NodeKind::Import, r"Vendor\Package\Tool");
    let boot = assert_node(&result.nodes, NodeKind::Function, "boot");
    assert_eq!(boot.return_type.as_deref(), Some("Tool"));
    assert_node(&result.nodes, NodeKind::Class, "Greeter");
    assert_node(&result.nodes, NodeKind::Method, "greet");
}

#[test]
fn batch_b_extensions_route_to_expected_languages() {
    assert_eq!(detect_language("Main.java"), Language::Java);
    assert_eq!(detect_language("user.c"), Language::C);
    assert_eq!(detect_language("widget.cpp"), Language::Cpp);
    assert_eq!(detect_language("Greeter.cs"), Language::CSharp);
    assert_eq!(detect_language("greeter.rb"), Language::Ruby);
    assert_eq!(detect_language("index.php"), Language::Php);
}

fn assert_node<'a>(nodes: &'a [Node], kind: NodeKind, name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}; nodes={nodes:#?}"))
}

fn assert_ref(refs: &[codegraph_core::types::UnresolvedRef], kind: EdgeKind, name: &str) {
    assert!(
        refs.iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing {kind:?} {name}; refs={refs:#?}"
    );
}
