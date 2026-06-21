//! JavaScript `LanguageSpec`.
//!
//! Direct port of `upstream extraction/languages/javascript.ts:4-84`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct JavaScriptSpec;

pub static JAVASCRIPT_SPEC: JavaScriptSpec = JavaScriptSpec;

impl LanguageSpec for JavaScriptSpec {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[
            "function_declaration",
            "arrow_function",
            "function_expression",
        ]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["method_definition", "field_definition"]
    }

    fn class_member_is_method(&self, node: Node<'_>, _source: &str) -> bool {
        crate::lang::typescript::class_field_is_callable(node, "field_definition")
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_statement"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["lexical_declaration", "variable_declaration"]
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
        ""
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        if node.kind() != "field_definition" {
            return None;
        }
        resolve_class_field_body(node, body_field)
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        child_by_field(node, "parameters").map(|params| node_text(params, source))
    }

    fn is_exported(&self, node: Node<'_>, _source: &str) -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "export_statement" {
                return true;
            }
            current = parent.parent();
        }
        false
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        has_direct_child_kind(node, "async")
    }

    fn is_const(&self, node: Node<'_>) -> bool {
        node.kind() == "lexical_declaration" && has_direct_child_kind(node, "const")
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let source_node = child_by_field(node, "source")?;
        let module_name = node_text(source_node, source).replace(['\'', '"'], "");
        if module_name.is_empty() {
            return None;
        }
        Some(ImportInfo {
            module_name,
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}

pub(crate) fn resolve_class_field_body<'tree>(
    node: Node<'tree>,
    body_field: &str,
) -> Option<Node<'tree>> {
    for i in 0..node.named_child_count() {
        let child = node.named_child(i as u32)?;
        if child.kind() == "arrow_function" || child.kind() == "function_expression" {
            return child_by_field(child, body_field);
        }
        if child.kind() == "call_expression" {
            if let Some(args) = child_by_field(child, "arguments") {
                for j in 0..args.named_child_count() {
                    if let Some(arg) = args.named_child(j as u32) {
                        if arg.kind() == "arrow_function" || arg.kind() == "function_expression" {
                            return child_by_field(arg, body_field);
                        }
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn has_direct_child_kind(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count()).any(|i| {
        node.child(i as u32)
            .is_some_and(|child| child.kind() == kind)
    })
}
