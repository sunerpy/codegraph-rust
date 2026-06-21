//! Java `LanguageSpec`.
//!
//! Direct port of `upstream extraction/languages/java.ts:5-106`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct JavaSpec;

pub static JAVA_SPEC: JavaSpec = JavaSpec;

impl LanguageSpec for JavaSpec {
    fn language(&self) -> Language {
        Language::Java
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_java::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method_declaration", "constructor_declaration"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration", "annotation_type_declaration"]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_constant"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["method_invocation"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["local_variable_declaration"]
    }
    fn field_types(&self) -> &'static [&'static str] {
        &["field_declaration"]
    }
    fn package_types(&self) -> &'static [&'static str] {
        &["package_declaration"]
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
        "type"
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let params_text = node_text(params, source);
        Some(match child_by_field(node, "type") {
            Some(return_type) => format!("{} {params_text}", node_text(return_type, source)),
            None => params_text,
        })
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let type_node = child_by_field(node, "type")?;
        if matches!(
            type_node.kind(),
            "void_type" | "integral_type" | "floating_point_type" | "boolean_type" | "array_type"
        ) {
            return None;
        }
        let raw = strip_angle_args(&node_text(type_node, source));
        let last = raw.split('.').next_back()?.trim();
        valid_ident(last).then(|| last.to_string())
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        modifier_kinds(node).and_then(|kinds| {
            if kinds.iter().any(|kind| *kind == "public") {
                Some("public".to_string())
            } else if kinds.iter().any(|kind| *kind == "private") {
                Some("private".to_string())
            } else if kinds.iter().any(|kind| *kind == "protected") {
                Some("protected".to_string())
            } else {
                None
            }
        })
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        modifier_kinds(node).is_some_and(|kinds| kinds.iter().any(|kind| *kind == "static"))
    }

    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        node.named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
            .map(|id| node_text(id, source).trim().to_string())
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let import_text = node_text(node, source).trim().to_string();
        let scoped = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "scoped_identifier")?;
        Some(ImportInfo {
            module_name: node_text(scoped, source),
            signature: import_text,
            handled_refs: false,
        })
    }
}

fn modifier_kinds(node: Node<'_>) -> Option<Vec<&str>> {
    let modifiers = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "modifiers")?;
    Some(
        modifiers
            .children(&mut modifiers.walk())
            .map(|child| child.kind())
            .collect(),
    )
}

fn strip_angle_args(input: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in input.trim().chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn valid_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
