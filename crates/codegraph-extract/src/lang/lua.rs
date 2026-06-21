//! Lua `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/lua.ts:8-152`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::{child_by_field, node_text};

pub struct LuaSpec;

pub static LUA_SPEC: LuaSpec = LuaSpec;

impl LanguageSpec for LuaSpec {
    fn language(&self) -> Language {
        Language::Lua
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_lua::LANGUAGE.into()
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
        &[]
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

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        child_by_field(node, "parameters").map(|params| node_text(params, source))
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
