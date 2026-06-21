use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::{child_by_field, node_text};

pub struct GoSpec;

pub static GO_SPEC: GoSpec = GoSpec;

impl LanguageSpec for GoSpec {
    fn language(&self) -> Language {
        Language::Go
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_go::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method_declaration"]
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
        &["type_spec"]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &[
            "var_declaration",
            "short_var_declaration",
            "const_declaration",
        ]
    }
    fn methods_are_top_level(&self) -> bool {
        true
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
        "result"
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut signature = node_text(params, source);
        if let Some(result) = child_by_field(node, "result") {
            signature.push(' ');
            signature.push_str(&node_text(result, source));
        }
        Some(signature)
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut result = child_by_field(node, "result")?;
        if result.kind() == "parameter_list" {
            let first = result
                .named_children(&mut result.walk())
                .find(|child| child.kind() == "parameter_declaration")?;
            result = child_by_field(first, "type").unwrap_or(first);
        }
        if result.kind() == "pointer_type" {
            result = result
                .named_children(&mut result.walk())
                .find(|child| {
                    matches!(
                        child.kind(),
                        "type_identifier" | "qualified_type" | "generic_type"
                    )
                })
                .unwrap_or(result);
        }
        let text = node_text(result, source)
            .trim()
            .trim_start_matches('*')
            .split(['[', '<'])
            .next()
            .unwrap_or_default()
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        is_identifier(&text).then_some(text)
    }

    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        let type_child = child_by_field(node, "type")?;
        match type_child.kind() {
            "struct_type" => Some(NodeKind::Struct),
            "interface_type" => Some(NodeKind::Interface),
            _ => None,
        }
    }

    fn is_exported(&self, node: Node<'_>, source: &str) -> bool {
        child_by_field(node, "name")
            .and_then(|name| node_text(name, source).bytes().next())
            .is_some_and(|first| first.is_ascii_uppercase())
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let receiver = child_by_field(node, "receiver")?;
        receiver_type_from_text(&node_text(receiver, source))
    }
}

fn receiver_type_from_text(text: &str) -> Option<String> {
    let inner = text.trim().trim_start_matches('(').trim();
    let parts = inner.split_whitespace().collect::<Vec<_>>();
    let candidate = if parts.len() >= 2 {
        parts[1]
    } else {
        parts.first()?
    };
    let clean = candidate
        .trim_start_matches('*')
        .split(['[', ')'])
        .next()
        .unwrap_or_default()
        .trim();
    is_identifier(clean).then_some(clean.to_string())
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
