//! ArkTS (HarmonyOS / OpenHarmony `.ets`) `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/arkts.ts` (#1186, behaviour as of
//! `b635dd46`). The ArkTS grammar is a TypeScript-superset fork, so this spec
//! mirrors [`crate::lang::typescript::TYPESCRIPT_SPEC`] except that:
//!
//! - the grammar handle is `tree-sitter-arkts` (which understands the ArkUI
//!   `@Component struct` syntax `tree-sitter-typescript` cannot parse);
//! - `struct_types` returns `["struct_declaration"]` so a `@Component struct`
//!   extracts as a [`codegraph_core::types::NodeKind::Struct`];
//! - the build() DSL's component expressions and leading-dot chains are call
//!   sites (`call_types`): the walker's ArkTS branch of `extract_call` names
//!   a component by its plain name and a chained attribute with a leading
//!   dot (`.width`), which only an attribute helper a decorator marks can
//!   resolve to; and
//! - `extract_modifiers` puts a struct's, field's, method's or function's
//!   decorator names on its node (upstream `collectDecoratorNames`), which
//!   the attribute strategy and the ArkUI synthesis passes read.
//!
//! tree-sitter-arkts 0.3.0 (upstream vendors 0.2.0) has no
//! `arkui_dsl_decorator_member_expression`, gives a `leading_dot_expression`
//! an `expression` field, and keeps a chain written after a nested
//! component's `}` on that component's one `arkui_component_expression`.

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
        &[
            "call_expression",
            "arkui_component_expression",
            "leading_dot_expression",
        ]
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

    fn extract_modifiers(&self, node: Node<'_>, source: &str) -> Vec<String> {
        if !matches!(
            node.kind(),
            "struct_declaration"
                | "public_field_definition"
                | "method_definition"
                | "function_declaration"
        ) {
            return Vec::new();
        }
        collect_decorator_names(node, source)
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
            let child = node.child(i)?;
            if child.kind() == "accessibility_modifier" {
                for j in 0..child.child_count() {
                    let modifier = child.child(j)?;
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
        // TypeScript's ambient module bodies, as the TypeScript spec reads
        // them (upstream #2418: ArkTS shares the TypeScript extractor).
        super::typescript::is_ambient_export(node)
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
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|child| child.kind() == kind))
}

/// The name a decorator gives (upstream `collectDecoratorNames`' `nameOf`):
/// its identifier, or its call's callee (`@StorageLink('theme')`,
/// `@Extend(Text)`).
fn decorator_name(decorator: Node<'_>, source: &str) -> Option<String> {
    for i in 0..decorator.named_child_count() {
        let Some(child) = decorator.named_child(i as u32) else {
            continue;
        };
        match child.kind() {
            "identifier" => return Some(node_text(child, source)),
            "call_expression" => {
                if let Some(function) =
                    child_by_field(child, "function").filter(|f| f.kind() == "identifier")
                {
                    return Some(node_text(function, source));
                }
            }
            _ => {}
        }
    }
    None
}

/// `node`'s decorator names (upstream `collectDecoratorNames`): the run of
/// `decorator` siblings right before it (a method's in a struct body, an
/// exported struct's on its `export` statement), then its own `decorator`
/// children, each in source order.
fn collect_decorator_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32)
            && child.kind() == "decorator"
            && let Some(name) = decorator_name(child, source)
        {
            names.push(name);
        }
    }
    let Some(parent) = node.parent() else {
        return names;
    };
    let siblings: Vec<Node<'_>> = (0..parent.named_child_count())
        .filter_map(|i| parent.named_child(i as u32))
        .collect();
    let Some(index) = siblings
        .iter()
        .position(|sibling| sibling.start_byte() == node.start_byte())
    else {
        return names;
    };
    let mut before: Vec<String> = Vec::new();
    for sibling in siblings[..index].iter().rev() {
        if sibling.kind() != "decorator" {
            break;
        }
        if let Some(name) = decorator_name(*sibling, source) {
            before.push(name);
        }
    }
    before.reverse();
    before.extend(names);
    before
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
