//! Objective-C `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/objc.ts:5-136`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct ObjCSpec;

pub static OBJC_SPEC: ObjCSpec = ObjCSpec;

impl LanguageSpec for ObjCSpec {
    fn language(&self) -> Language {
        Language::ObjC
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_objc::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_interface"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["method_definition"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &["protocol_declaration"]
    }

    fn interface_kind(&self) -> NodeKind {
        NodeKind::Protocol
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_specifier"]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_specifier"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enumerator"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_definition"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["preproc_include"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression", "message_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["declaration"]
    }

    fn property_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }

    fn name_field(&self) -> &'static str {
        "declarator"
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

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        extract_objc_method_name(node, source)
    }

    fn extract_property_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        extract_objc_property_name(node, source)
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        child_by_field(node, body_field).or_else(|| {
            node.named_children(&mut node.walk())
                .find(|child| child.kind() == "compound_statement")
        })
    }

    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "enum_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Enum);
            }
            if child.kind() == "struct_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Struct);
            }
        }
        None
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        (0..node.child_count()).any(|i| node.child(i as u32).is_some_and(|c| c.kind() == "+"))
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = node_text(node, source).trim().to_string();
        if let Some(system_lib) = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "system_lib_string")
        {
            return Some(ImportInfo {
                module_name: node_text(system_lib, source)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
                signature,
                handled_refs: false,
            });
        }
        let string_literal = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "string_literal")?;
        let content = string_literal
            .named_children(&mut string_literal.walk())
            .find(|child| child.kind() == "string_content")?;
        Some(ImportInfo {
            module_name: node_text(content, source),
            signature,
            handled_refs: false,
        })
    }
}

fn extract_objc_method_name(node: Node<'_>, source: &str) -> Option<String> {
    if !matches!(node.kind(), "method_definition" | "method_declaration") {
        return None;
    }
    let identifiers = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "identifier")
        .collect::<Vec<_>>();
    let first = identifiers.first().copied()?;
    let has_parameters = node
        .named_children(&mut node.walk())
        .any(|child| child.kind() == "method_parameter");
    if !has_parameters {
        return Some(node_text(first, source));
    }
    Some(
        identifiers
            .into_iter()
            .map(|id| format!("{}:", node_text(id, source)))
            .collect(),
    )
}

fn extract_objc_property_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "property_declaration" {
        return None;
    }
    let struct_decl = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "struct_declaration")?;
    let mut current = struct_decl
        .named_children(&mut struct_decl.walk())
        .find(|child| child.kind() == "struct_declarator")?;
    loop {
        let inner = child_by_field(current, "declarator").or_else(|| {
            current
                .named_children(&mut current.walk())
                .find(|child| matches!(child.kind(), "identifier" | "pointer_declarator"))
        })?;
        if inner.kind() == "identifier" {
            return Some(node_text(inner, source));
        }
        current = inner;
    }
}
