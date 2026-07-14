//! ArkTS (HarmonyOS / OpenHarmony `.ets`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/languages/arkts.ts` (commit
//! `9915221`). The ArkTS grammar is a TypeScript-superset fork, so this spec
//! mirrors [`crate::lang::typescript::TYPESCRIPT_SPEC`] verbatim except for two
//! things:
//!
//! - the grammar handle is `tree-sitter-arkts` (which understands the ArkUI
//!   `@Component struct` syntax `tree-sitter-typescript` cannot parse); and
//! - `struct_types` returns `["struct_declaration"]` so a `@Component struct`
//!   extracts as a [`codegraph_core::types::NodeKind::Struct`].
//!
//! The ArkUI dynamic-dispatch / callback-synthesizer bridges (attribute-chain
//! resolution, `arkui_component_expression` component-instantiation edges,
//! state->build() re-render, emitter emit->subscriber pairing, router->`@Entry`,
//! the decorator `extractModifiers` hook) are DELIBERATELY DEFERRED — the port
//! has no callback synthesizer. Hence `call_types` is `["call_expression"]`
//! only and `extract_modifiers` is left at the trait default.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::lang::typescript::class_field_is_callable;
use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct ArkTsSpec;

pub static ARKTS_SPEC: ArkTsSpec = ArkTsSpec;

impl LanguageSpec for ArkTsSpec {
    fn language(&self) -> Language {
        Language::ArkTs
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_arkts::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[
            "function_declaration",
            "arrow_function",
            "function_expression",
        ]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration", "abstract_class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["method_definition", "public_field_definition"]
    }

    fn class_member_is_method(&self, node: Node<'_>, _source: &str) -> bool {
        class_field_is_callable(node, "public_field_definition")
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
        &["property_identifier", "enum_assignment"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_alias_declaration"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_statement"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["lexical_declaration", "variable_declaration"]
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

    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        if node.kind() != "public_field_definition" {
            return None;
        }

        for i in 0..node.named_child_count() {
            let child = node.named_child(i as u32)?;
            if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                return child_by_field(child, body_field);
            }
            if child.kind() == "call_expression" {
                if let Some(args) = child_by_field(child, "arguments") {
                    for j in 0..args.named_child_count() {
                        if let Some(arg) = args.named_child(j as u32) {
                            if arg.kind() == "arrow_function" || arg.kind() == "function_expression"
                            {
                                return child_by_field(arg, body_field);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut signature = node_text(params, source);
        if let Some(return_type) = child_by_field(node, "return_type") {
            signature.push_str(": ");
            signature.push_str(
                node_text(return_type, source)
                    .trim_start_matches(':')
                    .trim_start(),
            );
        }
        Some(signature)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for i in 0..node.child_count() {
            let child = node.child(i as u32)?;
            if child.kind() == "accessibility_modifier" {
                for j in 0..child.child_count() {
                    let modifier = child.child(j as u32)?;
                    match modifier.kind() {
                        "public" => return Some("public".to_string()),
                        "private" => return Some("private".to_string()),
                        "protected" => return Some("protected".to_string()),
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn is_exported(&self, node: Node<'_>, _source: &str) -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "export_statement" {
                return true;
            }
            current = parent.parent();
        }
        false
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        has_direct_child_kind(node, "async")
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        has_direct_child_kind(node, "static")
    }

    fn is_const(&self, node: Node<'_>) -> bool {
        node.kind() == "lexical_declaration" && has_direct_child_kind(node, "const")
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let source_node = child_by_field(node, "source")?;
        let module_name = node_text(source_node, source).replace(['\'', '"'], "");
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

fn has_direct_child_kind(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count()).any(|i| {
        node.child(i as u32)
            .is_some_and(|child| child.kind() == kind)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_arkts::LANGUAGE.into())
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
    fn arkts_struct_type_is_struct_declaration() {
        assert_eq!(ARKTS_SPEC.struct_types(), ["struct_declaration"]);
    }

    #[test]
    fn arkts_field_constants_match_ts() {
        assert_eq!(ARKTS_SPEC.name_field(), "name");
        assert_eq!(ARKTS_SPEC.body_field(), "body");
        assert_eq!(ARKTS_SPEC.params_field(), "parameters");
        assert_eq!(ARKTS_SPEC.return_field(), "return_type");
    }

    #[test]
    fn arkts_parses_component_struct() {
        let src = "@Component struct MyView { build() { } }";
        let tree = parse(src);
        let decl = first_of_kind(tree.root_node(), "struct_declaration")
            .expect("tree-sitter-arkts parses @Component struct");
        let name = child_by_field(decl, "name").expect("struct has a name field");
        assert_eq!(node_text(name, src), "MyView");
    }
}
