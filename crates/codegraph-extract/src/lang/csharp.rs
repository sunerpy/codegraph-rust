//! C# `LanguageSpec`, ported from `upstream extraction/languages/csharp.ts:26-136`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct CSharpSpec;

pub static CSHARP_SPEC: CSharpSpec = CSharpSpec;

impl LanguageSpec for CSharpSpec {
    fn language(&self) -> Language {
        Language::CSharp
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_c_sharp::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration", "record_declaration"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method_declaration", "constructor_declaration"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration"]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_declaration", "record_struct_declaration"]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_member_declaration"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["using_directive"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["invocation_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["local_declaration_statement"]
    }
    fn field_types(&self) -> &'static [&'static str] {
        &["field_declaration"]
    }
    fn property_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }
    fn package_types(&self) -> &'static [&'static str] {
        &["namespace_declaration", "file_scoped_namespace_declaration"]
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
        "returns"
    }
    fn pre_parse(&self, source: &str) -> String {
        blank_csharp_preprocessor_directives(source)
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let type_node = child_by_field(node, "returns")?;
        if matches!(type_node.kind(), "predefined_type" | "array_type") {
            return None;
        }
        let text = strip_angle_args(&node_text(type_node, source))
            .trim_end_matches('?')
            .to_string();
        let last = text.split('.').next_back()?.trim();
        valid_ident(last).then(|| last.to_string())
    }
    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for child in node.children(&mut node.walk()) {
            if child.kind() == "modifier" {
                let kind = child.child(0).map_or(child.kind(), |inner| inner.kind());
                if matches!(kind, "public" | "private" | "protected" | "internal") {
                    return Some(kind.to_string());
                }
            }
        }
        Some("private".to_string())
    }
    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        has_modifier(node, "static")
    }
    fn is_async(&self, node: Node<'_>) -> bool {
        has_modifier(node, "async")
    }
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        child_by_field(node, "name")
            .or_else(|| {
                node.named_children(&mut node.walk())
                    .find(|c| matches!(c.kind(), "qualified_name" | "identifier"))
            })
            .map(|name| node_text(name, source))
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let module = node
            .named_children(&mut node.walk())
            .find(|c| matches!(c.kind(), "qualified_name" | "identifier"))?;
        Some(ImportInfo {
            module_name: node_text(module, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}

fn blank_csharp_preprocessor_directives(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if ["#if", "#elif", "#else", "#endif"]
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
            {
                line.chars()
                    .map(|ch| if ch == '\t' { '\t' } else { ' ' })
                    .collect::<String>()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_modifier(node: Node<'_>, modifier: &str) -> bool {
    node.children(&mut node.walk()).any(|child| {
        child.kind() == "modifier" && child.child(0).is_some_and(|inner| inner.kind() == modifier)
    })
}

fn strip_angle_args(input: &str) -> String {
    input.split('<').next().unwrap_or(input).trim().to_string()
}

fn valid_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
