//! GDScript (`.gd`) `LanguageSpec`.
//!
//! Non-upstream Rust-side addition. T3 implements function/method/constructor
//! and call extraction; classes, enums, variables, and extends/preload edges
//! are filled in by later todos.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::{child_by_field, node_text};

pub struct GdscriptSpec;

pub static GDSCRIPT_SPEC: GdscriptSpec = GdscriptSpec;

impl LanguageSpec for GdscriptSpec {
    fn language(&self) -> Language {
        Language::Gdscript
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_gdscript::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition", "constructor_definition"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_definition"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["function_definition", "constructor_definition"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_definition"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "body"
    }

    fn params_field(&self) -> &'static str {
        "parameters"
    }

    fn return_field(&self) -> &'static str {
        "return_type"
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, _body_field: &str) -> Option<Node<'tree>> {
        match node.kind() {
            "class_definition" => child_by_field(node, "class_body"),
            "enum_definition" => node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "enumerator_list"),
            _ => None,
        }
    }

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() != "constructor_definition" {
            return None;
        }
        let mut cursor = node.walk();
        let ctor = node
            .children(&mut cursor)
            .find(|child| child.kind() == "_init")
            .map(|tok| node_text(tok, source));
        ctor
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        let mut cursor = node.walk();
        let has_static = node
            .children(&mut cursor)
            .any(|child| child.kind() == "static_keyword");
        has_static
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        child_by_field(node, "parameters").map(|params| node_text(params, source))
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        child_by_field(node, "return_type").map(|ret| node_text(ret, source))
    }
}
