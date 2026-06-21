//! C `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:98-142`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct CSpec;

pub static C_SPEC: CSpec = CSpec;

impl LanguageSpec for CSpec {
    fn language(&self) -> Language {
        Language::C
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_c::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &[]
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
        &["call_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["declaration"]
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
        "type"
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        normalize_c_return_type(&node_text(child_by_field(node, "type")?, source))
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
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        include_import(node, source)
    }
}

pub(crate) fn include_import(node: Node<'_>, source: &str) -> Option<ImportInfo> {
    let signature = node_text(node, source).trim().to_string();
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "system_lib_string" {
            return Some(ImportInfo {
                module_name: node_text(child, source)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
                signature,
                handled_refs: false,
            });
        }
        if child.kind() == "string_literal" {
            if let Some(content) = child
                .named_children(&mut child.walk())
                .find(|c| c.kind() == "string_content")
            {
                return Some(ImportInfo {
                    module_name: node_text(content, source),
                    signature,
                    handled_refs: false,
                });
            }
        }
    }
    None
}

pub(crate) fn normalize_c_return_type(raw: &str) -> Option<String> {
    let mut text = raw.trim().to_string();
    for wrapper in ["unique_ptr", "shared_ptr", "weak_ptr", "optional"] {
        if let Some(start) = text.find(&format!("{wrapper}<")) {
            let inner_start = start + wrapper.len() + 1;
            if let Some(end) = text[inner_start..].find(['>', ',']) {
                text = text[inner_start..inner_start + end].to_string();
            }
        }
    }
    let cleaned = text
        .replace('*', " ")
        .replace('&', " ")
        .split_whitespace()
        .filter(|part| {
            !matches!(
                *part,
                "const" | "volatile" | "typename" | "struct" | "class" | "enum"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let last = cleaned.rsplit("::").next()?.trim();
    if last.is_empty() || CPP_NON_CLASS_RETURN.contains(&last) || !valid_ident(last) {
        return None;
    }
    Some(last.to_string())
}

const CPP_NON_CLASS_RETURN: [&str; 28] = [
    "void",
    "bool",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "unsigned",
    "signed",
    "size_t",
    "ssize_t",
    "auto",
    "wchar_t",
    "char8_t",
    "char16_t",
    "char32_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "intptr_t",
    "uintptr_t",
    "nullptr_t",
];

fn valid_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
