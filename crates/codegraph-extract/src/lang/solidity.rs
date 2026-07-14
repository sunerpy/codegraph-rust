//! Solidity (`.sol`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/languages/solidity.ts` (commit
//! `1441933`, #1170). Backed by the `tree-sitter-solidity` grammar crate, this
//! spec maps the Solidity node kinds to [`codegraph_core::types::NodeKind`]:
//!
//! - `contract_declaration` / `library_declaration` → [`NodeKind::Class`],
//!   `interface_declaration` → [`NodeKind::Interface`],
//!   `struct_declaration` → [`NodeKind::Struct`],
//!   `enum_declaration` → [`NodeKind::Enum`];
//! - `function_definition` / `modifier_definition` are functions/methods, and the
//!   nameless `constructor_definition` / `fallback_receive_definition` get the
//!   synthetic names `"constructor"` / `"fallback"` / `"receive"`;
//! - `is`-inheritance is emitted as `Extends` refs by the walker (D3) — the
//!   EXISTING resolver promotes those to `Implements` for interface targets, so
//!   this spec adds NO resolve-tier code.
//!
//! The Solidity-only AST shapes the generic dispatcher cannot handle
//! (direct-`name` fields, bare-text `enum_value`, header `modifier_invocation`,
//! file-level const, file-level `event`/`error`, and the `user_defined_type`
//! inheritance ancestor) are handled by the `Language::Solidity`-guarded walker
//! extensions in [`crate::walker`].

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct SoliditySpec;

pub static SOLIDITY_SPEC: SoliditySpec = SoliditySpec;

impl LanguageSpec for SoliditySpec {
    fn language(&self) -> Language {
        Language::Solidity
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_solidity::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition", "modifier_definition"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["contract_declaration", "library_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &[
            "function_definition",
            "modifier_definition",
            "constructor_definition",
            "fallback_receive_definition",
        ]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration"]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_declaration"]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_value"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["user_defined_type_definition"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_directive"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &[
            "call_expression",
            "emit_statement",
            "revert_statement",
            "modifier_invocation",
        ]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[
            "state_variable_declaration",
            "constant_variable_declaration",
        ]
    }

    fn field_types(&self) -> &'static [&'static str] {
        &[
            "state_variable_declaration",
            "struct_member",
            "event_definition",
            "error_declaration",
        ]
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

    fn resolve_name(&self, node: Node<'_>, _source: &str) -> Option<String> {
        match node.kind() {
            "constructor_definition" => Some("constructor".to_string()),
            "fallback_receive_definition" => {
                // The keyword is an anonymous child (`fallback` or `receive`);
                // walk every child and return the first that matches, defaulting
                // to `fallback` (upstream `fallbackReceiveName`).
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        match child.kind() {
                            "fallback" => return Some("fallback".to_string()),
                            "receive" => return Some("receive".to_string()),
                            _ => {}
                        }
                    }
                }
                Some("fallback".to_string())
            }
            _ => None,
        }
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Params are DIRECT children of the decl node, not a `parameters:` field.
        // Walk the named children and reassemble `(p1, p2) <vis> <mut> <ret>`.
        let mut params: Vec<String> = Vec::new();
        let mut visibility: Option<String> = None;
        let mut mutability: Option<String> = None;
        let mut return_type: Option<String> = None;
        for child in node.named_children(&mut node.walk()) {
            match child.kind() {
                "parameter" => params.push(node_text(child, source).trim().to_string()),
                "visibility" => visibility = Some(node_text(child, source).trim().to_string()),
                "state_mutability" => {
                    mutability = Some(node_text(child, source).trim().to_string())
                }
                "return_type_definition" => {
                    return_type = Some(node_text(child, source).trim().to_string())
                }
                _ => {}
            }
        }
        let mut signature = format!("({})", params.join(", "));
        if let Some(visibility) = visibility {
            signature.push(' ');
            signature.push_str(&visibility);
        }
        if let Some(mutability) = mutability {
            signature.push(' ');
            signature.push_str(&mutability);
        }
        if let Some(return_type) = return_type {
            signature.push(' ');
            signature.push_str(&return_type);
        }
        Some(signature)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "visibility" {
                let text = child
                    .child(0)
                    .map(|kw| kw.kind().to_string())
                    .unwrap_or_default();
                return match text.as_str() {
                    "public" | "external" => Some("public".to_string()),
                    "private" => Some("private".to_string()),
                    "internal" => Some("internal".to_string()),
                    _ => None,
                };
            }
        }
        None
    }

    fn is_const(&self, node: Node<'_>) -> bool {
        node.kind() == "constant_variable_declaration"
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let source_node = child_by_field(node, "source")?;
        let module_name = node_text(source_node, source)
            .trim()
            .trim_matches(['\'', '"'])
            .to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_solidity::LANGUAGE.into())
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
    fn solidity_class_types() {
        assert_eq!(
            SOLIDITY_SPEC.class_types(),
            ["contract_declaration", "library_declaration"]
        );
    }

    #[test]
    fn solidity_call_types_include_emit_revert_modifier() {
        assert_eq!(
            SOLIDITY_SPEC.call_types(),
            [
                "call_expression",
                "emit_statement",
                "revert_statement",
                "modifier_invocation"
            ]
        );
    }

    #[test]
    fn solidity_field_types_include_event_error() {
        assert_eq!(
            SOLIDITY_SPEC.field_types(),
            [
                "state_variable_declaration",
                "struct_member",
                "event_definition",
                "error_declaration"
            ]
        );
    }

    #[test]
    fn solidity_parses_contract() {
        let src = "contract A is B { function f() public {} }";
        let tree = parse(src);
        let contract = first_of_kind(tree.root_node(), "contract_declaration")
            .expect("tree-sitter-solidity parses a contract");
        let name = child_by_field(contract, "name").expect("contract has a name field");
        assert_eq!(node_text(name, src), "A");
        assert!(
            first_of_kind(tree.root_node(), "inheritance_specifier").is_some(),
            "the `is B` clause parses as an inheritance_specifier"
        );
    }

    #[test]
    fn solidity_resolve_name_constructor() {
        let src = "contract C { constructor() {} }";
        let tree = parse(src);
        let ctor = first_of_kind(tree.root_node(), "constructor_definition")
            .expect("parses a constructor_definition");
        assert_eq!(
            SOLIDITY_SPEC.resolve_name(ctor, src),
            Some("constructor".to_string())
        );
    }

    #[test]
    fn solidity_resolve_name_fallback_receive() {
        let src = "contract C { fallback() external {} receive() external payable {} }";
        let tree = parse(src);
        let root = tree.root_node();
        let mut names = Vec::new();
        collect_kind(root, "fallback_receive_definition", &mut names, src);
        assert!(names.contains(&"fallback".to_string()));
        assert!(names.contains(&"receive".to_string()));
    }

    fn collect_kind(node: Node<'_>, kind: &str, out: &mut Vec<String>, src: &str) {
        if node.kind() == kind {
            if let Some(name) = SOLIDITY_SPEC.resolve_name(node, src) {
                out.push(name);
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                collect_kind(child, kind, out, src);
            }
        }
    }
}
