//! Per-language extractor coverage for languages NOT covered by the sibling
//! `batch_coverage_languages.rs` file (which owns dart/scala/csharp/php/objc/
//! cpp/c/swift/typescript/javascript/ruby/pascal). This file targets the
//! remaining lower-coverage Tier-1 extractors: r, kotlin, rust, go, java,
//! python, lua, luau. Mirrors the `extract_source(file, source, Some(Language))`
//! pattern from `batch_a/b/c_languages.rs`. TEST-ONLY: no production change.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_extract::extract_source;

#[test]
fn r_extracts_calls_from_function_bodies() {
    let source = r#"
library(dplyr)
compute <- function(x) {
  helper(x)
  transform(x)
}
result <- compute(10)
"#;
    let result = extract_source("analysis/compute.R", source, Some(Language::R));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "compute");
}

#[test]
fn kotlin_data_class_object_interface_and_functions() {
    let source = r#"
package demo
import kotlin.collections.List
interface Greeter { fun greet(): String }
data class Person(val name: String)
object Registry {
    fun register(): Widget = build()
}
class Service : Greeter {
    override fun greet(): String = "hi"
    private suspend fun load(): Result = fetch()
}
"#;
    let result = extract_source("src/Service.kt", source, Some(Language::Kotlin));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Interface, "Greeter");
    assert_node(&result.nodes, NodeKind::Class, "Person");
    assert_node(&result.nodes, NodeKind::Method, "greet");
    assert_node(&result.nodes, NodeKind::Method, "register");
}

#[test]
fn rust_traits_impls_enums_generics_and_use() {
    let source = r#"
use std::collections::HashMap;
pub trait Greeter {
    fn greet(&self) -> String;
}
pub enum Color { Red, Green(u8), Blue { shade: u8 } }
pub struct Service<T> {
    items: Vec<T>,
}
impl Greeter for Service<String> {
    fn greet(&self) -> String {
        helper()
    }
}
pub async fn boot() -> Result<(), Error> {
    start().await
}
"#;
    let result = extract_source("src/service.rs", source, Some(Language::Rust));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Trait, "Greeter");
    assert_node(&result.nodes, NodeKind::Enum, "Color");
    assert_node(&result.nodes, NodeKind::Struct, "Service");
    assert_node(&result.nodes, NodeKind::Function, "boot");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
}

#[test]
fn go_structs_interfaces_methods_and_imports() {
    let source = r#"
package service
import (
    "fmt"
    "net/http"
)
type Greeter interface {
    Greet() string
}
type Service struct {
    name string
}
func (s *Service) Greet() string {
    return fmt.Sprintf("hi %s", s.name)
}
func Boot() error {
    return start()
}
"#;
    let result = extract_source("service/service.go", source, Some(Language::Go));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Interface, "Greeter");
    assert_node(&result.nodes, NodeKind::Struct, "Service");
    assert_node(&result.nodes, NodeKind::Method, "Greet");
    assert_node(&result.nodes, NodeKind::Function, "Boot");
}

#[test]
fn java_generics_enums_annotations_and_static() {
    let source = r#"
package com.example;
import java.util.List;
public enum Status { ACTIVE, INACTIVE }
public interface Repo<T> {
    T find(int id);
}
public class Service {
    private int count;
    public static Service create() { return new Service(); }
    public List<String> names() {
        return helper();
    }
}
"#;
    let result = extract_source("src/Service.java", source, Some(Language::Java));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Enum, "Status");
    assert_node(&result.nodes, NodeKind::Interface, "Repo");
    assert_node(&result.nodes, NodeKind::Class, "Service");
    let create = assert_node(&result.nodes, NodeKind::Method, "create");
    assert!(create.is_static, "create() should be static");
}

#[test]
fn python_decorators_nested_functions_and_async() {
    let source = r#"
import os
from typing import List

def outer():
    def inner():
        return helper()
    return inner

class Service:
    @staticmethod
    def build():
        return make()

    async def run(self):
        await fetch()
"#;
    let result = extract_source("src/service.py", source, Some(Language::Python));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Class, "Service");
    assert_node(&result.nodes, NodeKind::Function, "outer");
    assert_node(&result.nodes, NodeKind::Function, "inner");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "helper");
}

#[test]
fn lua_local_functions_table_methods_and_require() {
    let source = r#"
local util = require("app.util")
local M = {}
local function private_helper()
  compute()
end
function M.public_api(x)
  private_helper()
  return util.transform(x)
end
return M
"#;
    let result = extract_source("src/mod.lua", source, Some(Language::Lua));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::Import, "app.util");
    assert_node(&result.nodes, NodeKind::Method, "public_api");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "compute");
}

#[test]
fn luau_type_alias_export_and_typed_function() {
    let source = r#"
export type User = { name: string, age: number }
local Signal = require(script.Parent.Signal)
function Signal.create(name: string): User
  validate(name)
  return { name = name, age = 0 }
end
"#;
    let result = extract_source("src/signal.luau", source, Some(Language::Luau));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result.nodes, NodeKind::TypeAlias, "User");
    assert_node(&result.nodes, NodeKind::Import, "Signal");
    assert_node(&result.nodes, NodeKind::Method, "create");
    assert_ref(&result.unresolved_references, EdgeKind::Calls, "validate");
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
