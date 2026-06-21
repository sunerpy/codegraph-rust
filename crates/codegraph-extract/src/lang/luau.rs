//! Luau `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/luau.ts:5-36`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::{child_by_field, node_text};

pub struct LuauSpec;

pub static LUAU_SPEC: LuauSpec = LuauSpec;

impl LanguageSpec for LuauSpec {
    fn language(&self) -> Language {
        Language::Luau
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_luau::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
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
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_definition"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["function_call"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["variable_declaration"]
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

    fn is_exported(&self, node: Node<'_>, source: &str) -> bool {
        source.get(node.start_byte()..node.start_byte().saturating_add(7)) == Some("export ")
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut sig = node_text(params, source);
        let mut saw_params = false;
        for child in node.named_children(&mut node.walk()) {
            if child.start_byte() == params.start_byte() {
                saw_params = true;
                continue;
            }
            if saw_params && child.kind() != "block" {
                sig.push_str(": ");
                sig.push_str(&node_text(child, source));
                break;
            }
            if saw_params {
                break;
            }
        }
        Some(sig)
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let name = child_by_field(node, "name")?;
        if matches!(
            name.kind(),
            "dot_index_expression" | "method_index_expression"
        ) {
            child_by_field(name, "table").map(|table| node_text(table, source))
        } else {
            None
        }
    }
}
