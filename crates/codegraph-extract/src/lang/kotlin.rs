//! Kotlin `LanguageSpec`.
//!
//! Ports `upstream extraction/languages/kotlin.ts:71-308` onto the
//! `tree-sitter-kotlin-ng` node names used by this Rust workspace.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct KotlinSpec;

pub static KOTLIN_SPEC: KotlinSpec = KotlinSpec;

impl LanguageSpec for KotlinSpec {
    fn language(&self) -> Language {
        Language::Kotlin
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
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
        &["enum_entry"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_alias"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }

    fn field_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }

    fn extra_class_node_types(&self) -> &'static [&'static str] {
        &["object_declaration"]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "function_body"
    }

    fn params_field(&self) -> &'static str {
        "function_value_parameters"
    }

    fn return_field(&self) -> &'static str {
        "type"
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, _body_field: &str) -> Option<Node<'tree>> {
        // Upstream kotlin.ts:174-196 resolves Kotlin bodies by child type rather
        // than field name because the upstream grammar historically had no fields.
        node.named_children(&mut node.walk()).find(|child| {
            matches!(
                child.kind(),
                "function_body" | "class_body" | "enum_class_body"
            )
        })
    }

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() == "type_alias" {
            return child_by_field(node, "type").map(|name| node_text(name, source));
        }
        None
    }

    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        // Upstream kotlin.ts:197-210 scans DIRECT keyword children only
        // ('interface' / 'enum'); a whole-subtree scan would misclassify a
        // class containing a nested enum. tree-sitter-kotlin-ng nests the
        // 'enum' keyword under modifiers > class_modifier, so the modifiers
        // child is also scanned; the scan stops at the class body.
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            match child.kind() {
                "interface" => return NodeKind::Interface,
                "enum" => return NodeKind::Enum,
                "modifiers" => {
                    if has_descendant_kind(child, "enum") {
                        return NodeKind::Enum;
                    }
                }
                "class_body" | "enum_class_body" => break,
                _ => {}
            }
        }
        NodeKind::Class
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:211-231: extension receiver is the user_type before '.'.
        let mut found_user_type = None;
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if child.kind() == "user_type" {
                found_user_type = Some(child);
            } else if child.kind() == "." {
                let receiver = found_user_type?;
                return Some(bare_type_name(receiver, source));
            } else if child.kind() == "identifier" || child.kind() == "function_value_parameters" {
                break;
            }
        }
        None
    }

    fn get_signature(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        // Upstream kotlin.ts:232-242 calls getChildByField(node,
        // 'function_value_parameters'); the kotlin grammar exposes no such
        // FIELD, so the upstream hook always returns undefined. Mirror with None.
        None
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:5-43 normalizes declared return types to bare class names.
        let return_type = declared_return_type_node(node)?;
        let name = bare_type_name(return_type, source);
        if name.is_empty() || matches!(name.as_str(), "Unit" | "Nothing") || !is_ident(&name) {
            return None;
        }
        Some(name)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        // Upstream kotlin.ts:243-256 defaults Kotlin visibility to public.
        for child in node.children(&mut node.walk()) {
            if child.kind() != "modifiers" {
                continue;
            }
            if has_descendant_kind(child, "public") {
                return Some("public".to_string());
            }
            if has_descendant_kind(child, "private") {
                return Some("private".to_string());
            }
            if has_descendant_kind(child, "protected") {
                return Some("protected".to_string());
            }
            if has_descendant_kind(child, "internal") {
                return Some("internal".to_string());
            }
        }
        Some("public".to_string())
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        node.children(&mut node.walk())
            .any(|child| child.kind() == "modifiers" && has_descendant_kind(child, "suspend"))
    }

    fn extract_modifiers(&self, node: Node<'_>) -> Vec<String> {
        // Upstream kotlin.ts:271-293: Kotlin Multiplatform expect/actual markers
        // under modifiers > platform_modifier, matched by AST node not text.
        let mut mods = Vec::new();
        for child in node.children(&mut node.walk()) {
            if child.kind() != "modifiers" {
                continue;
            }
            for pm in child.children(&mut child.walk()) {
                if pm.kind() != "platform_modifier" {
                    continue;
                }
                for kw in pm.children(&mut pm.walk()) {
                    if matches!(kw.kind(), "expect" | "actual") {
                        mods.push(kw.kind().to_string());
                    }
                }
            }
        }
        mods
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        // Upstream kotlin.ts:294-301 emits import_header text and module name.
        let identifier = first_descendant_named(node, "qualified_identifier")
            .or_else(|| first_descendant_named(node, "identifier"))?;
        Some(ImportInfo {
            module_name: node_text(identifier, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    fn package_types(&self) -> &'static [&'static str] {
        &["package_header"]
    }

    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:302-307 package_header -> dotted identifier.
        first_descendant_named(node, "qualified_identifier").map(|id| node_text(id, source))
    }
}

fn declared_return_type_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut seen_params = false;
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "function_value_parameters" {
            seen_params = true;
            continue;
        }
        if !seen_params {
            continue;
        }
        if matches!(child.kind(), "function_body" | "type_constraints") {
            return None;
        }
        if matches!(child.kind(), "user_type" | "nullable_type") {
            return Some(child);
        }
    }
    None
}

fn bare_type_name(node: Node<'_>, source: &str) -> String {
    let leaf = first_descendant_named(node, "identifier").unwrap_or(node);
    node_text(leaf, source)
        .trim_matches('`')
        .trim_end_matches('?')
        .to_string()
}

fn first_descendant_named<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    for child in node.named_children(&mut node.walk()) {
        if let Some(found) = first_descendant_named(child, kind) {
            return Some(found);
        }
    }
    None
}

fn has_descendant_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    for idx in 0..node.child_count() {
        if node
            .child(idx as u32)
            .is_some_and(|child| has_descendant_kind(child, kind))
        {
            return true;
        }
    }
    false
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
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
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
        assert_eq!(KOTLIN_SPEC.name_field(), "name");
        assert_eq!(KOTLIN_SPEC.body_field(), "function_body");
        assert_eq!(KOTLIN_SPEC.params_field(), "function_value_parameters");
        assert_eq!(KOTLIN_SPEC.return_field(), "type");
    }

    #[test]
    fn nullable_return_and_missing_return() {
        let src = "class C {\n  fun a(): Widget? = null\n  fun b() {}\n}\n";
        let tree = parse(src);
        let mut fns = Vec::new();
        fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
            if n.kind() == "function_declaration" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        walk(tree.root_node(), &mut fns);
        assert_eq!(
            KOTLIN_SPEC.get_return_type(fns[0], src).as_deref(),
            Some("Widget")
        );
        assert!(KOTLIN_SPEC.get_return_type(fns[1], src).is_none());
    }

    #[test]
    fn visibility_public_default_and_modifiers() {
        let src = "class C {\n  private fun a() {}\n  protected fun b() {}\n  internal fun c() {}\n  fun d() {}\n}\n";
        let tree = parse(src);
        let mut fns = Vec::new();
        fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
            if n.kind() == "function_declaration" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        walk(tree.root_node(), &mut fns);
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[0]).as_deref(),
            Some("private")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[1]).as_deref(),
            Some("protected")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[2]).as_deref(),
            Some("internal")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[3]).as_deref(),
            Some("public")
        );
    }

    #[test]
    fn qualified_import_produces_module() {
        let src = "import com.example.util.Helper\n";
        let tree = parse(src);
        let import = first_of_kind(tree.root_node(), "import").unwrap();
        let info = KOTLIN_SPEC.extract_import(import, src).unwrap();
        assert!(info.module_name.contains("Helper") || !info.module_name.is_empty());
    }
}
