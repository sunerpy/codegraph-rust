//! Ruby `LanguageSpec`, ported from `upstream extraction/languages/ruby.ts:5-147`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::node_text;

pub struct RubySpec;

pub static RUBY_SPEC: RubySpec = RubySpec;

impl LanguageSpec for RubySpec {
    fn language(&self) -> Language {
        Language::Ruby
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_ruby::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["method"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method", "singleton_method"]
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
        &["call"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call", "method_call"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["assignment"]
    }
    fn module_types(&self) -> &'static [&'static str] {
        &["module"]
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
    fn get_visibility(&self, _node: Node<'_>) -> Option<String> {
        Some("public".to_string())
    }
    fn extract_bare_call(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() != "identifier" {
            return None;
        }
        let parent = node.parent()?;
        if !matches!(
            parent.kind(),
            "body_statement" | "then" | "else" | "do" | "begin" | "rescue" | "ensure" | "when"
        ) {
            return None;
        }
        let name = node_text(node, source);
        if matches!(
            name.as_str(),
            "true" | "false" | "nil" | "self" | "super" | "__FILE__" | "__LINE__" | "__dir__"
        ) {
            return None;
        }
        if name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
        {
            return None;
        }
        Some(name)
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let identifier = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "identifier")?;
        let method_name = node_text(identifier, source);
        if !matches!(method_name.as_str(), "require" | "require_relative") {
            return None;
        }
        let arg_list = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "argument_list")?;
        let string_node = arg_list
            .named_children(&mut arg_list.walk())
            .find(|child| child.kind() == "string")?;
        let content = string_node
            .named_children(&mut string_node.walk())
            .find(|child| child.kind() == "string_content")?;
        Some(ImportInfo {
            module_name: node_text(content, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}
