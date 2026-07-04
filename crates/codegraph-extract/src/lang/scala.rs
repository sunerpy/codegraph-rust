//! Scala `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/scala.ts:5-201`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct ScalaSpec;

pub static SCALA_SPEC: ScalaSpec = ScalaSpec;

impl LanguageSpec for ScalaSpec {
    fn language(&self) -> Language {
        Language::Scala
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_scala::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_definition", "object_definition", "trait_definition"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["function_definition", "function_declaration"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_definition"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_definition"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_declaration"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[]
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

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let rt = child_by_field(node, "return_type")?;
        let raw = node_text(rt, source).trim().to_string();
        if raw.starts_with("this.") {
            return None;
        }
        let base = strip_bracket_generics(&raw).replace(char::is_whitespace, "");
        let last = base.split('.').next_back()?.to_string();
        is_ident(&last).then_some(last)
    }

    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        if node.kind() == "trait_definition" {
            NodeKind::Trait
        } else {
            NodeKind::Class
        }
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters");
        let return_type = child_by_field(node, "return_type");
        if params.is_none() && return_type.is_none() {
            return None;
        }
        let mut sig = params.map(|p| node_text(p, source)).unwrap_or_default();
        if let Some(return_type) = return_type {
            sig.push_str(": ");
            sig.push_str(&node_text(return_type, source));
        }
        (!sig.is_empty()).then_some(sig)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for child in node.named_children(&mut node.walk()) {
            if matches!(child.kind(), "modifiers" | "access_modifier") {
                let has_private = (0..child.child_count()).any(|idx| {
                    child
                        .child(idx as u32)
                        .is_some_and(|inner| inner.kind() == "private")
                });
                let has_protected = (0..child.child_count()).any(|idx| {
                    child
                        .child(idx as u32)
                        .is_some_and(|inner| inner.kind() == "protected")
                });
                if has_private {
                    return Some("private".to_string());
                }
                if has_protected {
                    return Some("protected".to_string());
                }
            }
        }
        Some("public".to_string())
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        node.named_children(&mut node.walk()).any(|child| {
            child.kind() == "modifiers"
                && (0..child.child_count()).any(|idx| {
                    child
                        .child(idx as u32)
                        .is_some_and(|inner| inner.kind() == "static")
                })
        })
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = node_text(node, source).trim().to_string();
        if let Some(path) = child_by_field(node, "path") {
            return Some(ImportInfo {
                module_name: node_text(path, source),
                signature,
                handled_refs: false,
            });
        }
        for child in node.named_children(&mut node.walk()) {
            if matches!(child.kind(), "identifier" | "stable_identifier") {
                return Some(ImportInfo {
                    module_name: node_text(child, source),
                    signature,
                    handled_refs: false,
                });
            }
        }
        None
    }
}

fn strip_bracket_generics(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.named_child_count() {
            let child = node.named_child(i as u32)?;
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn trait_field_constants_are_stable() {
        assert_eq!(SCALA_SPEC.name_field(), "name");
        assert_eq!(SCALA_SPEC.body_field(), "body");
        assert_eq!(SCALA_SPEC.params_field(), "parameters");
        assert_eq!(SCALA_SPEC.return_field(), "return_type");
    }

    #[test]
    fn stable_identifier_import_fallback() {
        let src = "import a.b.c\n";
        let tree = parse(src);
        let import = first_of_kind(tree.root_node(), "import_declaration").unwrap();
        let info = SCALA_SPEC.extract_import(import, src).unwrap();
        assert!(!info.module_name.is_empty());
    }

    #[test]
    fn single_identifier_import_uses_fallback_branch() {
        let src = "import foo\n";
        let tree = parse(src);
        let import = first_of_kind(tree.root_node(), "import_declaration").unwrap();
        let info = SCALA_SPEC.extract_import(import, src).unwrap();
        assert_eq!(info.module_name, "foo");
    }

    #[test]
    fn generic_bracket_return_strips_to_outer_type() {
        let src = "class C { def f(): Option[String] = None }";
        let tree = parse(src);
        let func = first_of_kind(tree.root_node(), "function_definition").unwrap();
        assert_eq!(
            SCALA_SPEC.get_return_type(func, src).as_deref(),
            Some("Option")
        );
    }
}
