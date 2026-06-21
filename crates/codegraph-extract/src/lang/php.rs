//! PHP `LanguageSpec`, ported from `upstream extraction/languages/php.ts:5-189`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct PhpSpec;

pub static PHP_SPEC: PhpSpec = PhpSpec;

const INCLUDE_TYPES: [&str; 4] = [
    "include_expression",
    "include_once_expression",
    "require_expression",
    "require_once_expression",
];

impl LanguageSpec for PhpSpec {
    fn language(&self) -> Language {
        Language::Php
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_php::LANGUAGE_PHP.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration", "trait_declaration"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method_declaration"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration"]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_case"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &[
            "namespace_use_declaration",
            "include_expression",
            "include_once_expression",
            "require_expression",
            "require_once_expression",
        ]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &[
            "function_call_expression",
            "member_call_expression",
            "scoped_call_expression",
        ]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["const_declaration"]
    }
    fn field_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }
    fn package_types(&self) -> &'static [&'static str] {
        &["namespace_definition"]
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
    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        if node.kind() == "trait_declaration" {
            NodeKind::Trait
        } else {
            NodeKind::Class
        }
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut rt = child_by_field(node, "return_type")?;
        if rt.kind() == "optional_type" {
            rt = rt.named_child(0).unwrap_or(rt);
        }
        if rt.kind() == "primitive_type" {
            return None;
        }
        let name_node = if rt.kind() == "named_type" {
            rt.named_child(0).unwrap_or(rt)
        } else {
            rt
        };
        let text = node_text(name_node, source)
            .trim_start_matches('\\')
            .to_string();
        let last = text.split('\\').next_back()?.trim();
        let lower = last.to_ascii_lowercase();
        if matches!(lower.as_str(), "self" | "static" | "this" | "$this") {
            return Some("self".to_string());
        }
        if PHP_NON_CLASS_RETURN.contains(&lower.as_str()) || !valid_ident(last) {
            return None;
        }
        Some(last.to_string())
    }
    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for child in node.children(&mut node.walk()) {
            if child.kind() == "visibility_modifier" {
                return Some(
                    child
                        .child(0)
                        .map_or(child.kind(), |inner| inner.kind())
                        .to_string(),
                );
            }
        }
        Some("public".to_string())
    }
    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        node.children(&mut node.walk())
            .any(|child| child.kind() == "static_modifier")
    }
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node
            .named_children(&mut node.walk())
            .any(|c| matches!(c.kind(), "compound_statement" | "declaration_list"))
        {
            return None;
        }
        node.named_children(&mut node.walk())
            .find(|c| c.kind() == "namespace_name")
            .map(|name| node_text(name, source))
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = node_text(node, source).trim().to_string();
        if INCLUDE_TYPES.contains(&node.kind()) {
            return static_include_path(node, source).map(|module_name| ImportInfo {
                module_name,
                signature,
                handled_refs: false,
            });
        }
        let use_clause = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "namespace_use_clause")?;
        let name = use_clause
            .named_children(&mut use_clause.walk())
            .find(|c| matches!(c.kind(), "qualified_name" | "name"))?;
        Some(ImportInfo {
            module_name: node_text(name, source),
            signature,
            handled_refs: false,
        })
    }
}

const PHP_NON_CLASS_RETURN: [&str; 18] = [
    "array", "string", "int", "integer", "float", "double", "bool", "boolean", "void", "mixed",
    "never", "null", "false", "true", "object", "callable", "iterable", "resource",
];

fn static_include_path(node: Node<'_>, source: &str) -> Option<String> {
    let mut arg = node.named_child(0)?;
    if arg.kind() == "parenthesized_expression" {
        arg = arg.named_child(0)?;
    }
    if !matches!(arg.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut content = None;
    for child in arg.named_children(&mut arg.walk()) {
        if child.kind() != "string_content" {
            return None;
        }
        content = Some(node_text(child, source));
    }
    content
}

fn valid_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
