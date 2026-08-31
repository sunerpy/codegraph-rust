use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct RustSpec;

pub static RUST_SPEC: RustSpec = RustSpec;

/// Return the implementing type's simple name from an `impl_item` `type` field.
///
/// Generic, scoped, and reference wrappers are unwrapped recursively. Shapes
/// that do not name one concrete type (tuples, `dyn`, raw pointers, primitive
/// and function types) deliberately return `None`.
pub(crate) fn rust_impl_type_name(type_node: Option<Node<'_>>, source: &str) -> Option<String> {
    let type_node = type_node?;
    match type_node.kind() {
        "type_identifier" | "identifier" => Some(node_text(type_node, source)),
        "generic_type" | "reference_type" => {
            rust_impl_type_name(child_by_field(type_node, "type"), source)
        }
        "scoped_type_identifier" | "scoped_identifier" => {
            rust_impl_type_name(child_by_field(type_node, "name"), source)
        }
        _ => None,
    }
}

impl LanguageSpec for RustSpec {
    fn language(&self) -> Language {
        Language::Rust
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_rust::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_item", "function_signature_item"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["function_item", "function_signature_item"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &["trait_item"]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_item"]
    }
    fn union_types(&self) -> &'static [&'static str] {
        &["union_item"]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_item"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_variant"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_item"]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["use_declaration"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["let_declaration", "const_item", "static_item"]
    }
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Trait
    }
    fn allow_bodiless_struct(&self) -> bool {
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
        "return_type"
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut signature = node_text(params, source);
        if let Some(return_type) = child_by_field(node, "return_type") {
            signature.push_str(" -> ");
            signature.push_str(&node_text(return_type, source));
        }
        Some(signature)
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut return_type = child_by_field(node, "return_type")?;
        if return_type.kind() == "reference_type" {
            return_type = return_type
                .named_children(&mut return_type.walk())
                .find(|child| {
                    matches!(
                        child.kind(),
                        "type_identifier" | "scoped_type_identifier" | "generic_type"
                    )
                })
                .unwrap_or(return_type);
        }
        if matches!(
            return_type.kind(),
            "primitive_type" | "unit_type" | "tuple_type"
        ) {
            return None;
        }
        let text = node_text(return_type, source);
        let bare = text
            .trim()
            .split('<')
            .next()
            .unwrap_or_default()
            .rsplit("::")
            .next()
            .unwrap_or_default()
            .trim();
        if !is_identifier(bare) {
            return None;
        }
        Some(if bare == "Self" {
            "self".to_string()
        } else {
            bare.to_string()
        })
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        has_child_kind_recursive(node, "async")
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "visibility_modifier" {
                    return Some(
                        if (0..child.child_count()).any(|idx| {
                            child
                                .child(idx as u32)
                                .is_some_and(|inner| inner.kind() == "pub")
                        }) {
                            "public"
                        } else {
                            "private"
                        }
                        .to_string(),
                    );
                }
            }
        }
        Some("private".to_string())
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut parent = node.parent();
        while let Some(current) = parent {
            if current.kind() == "impl_item" {
                return rust_impl_type_name(child_by_field(current, "type"), source);
            }
            parent = current.parent();
        }
        None
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let import_text = node_text(node, source).trim().to_string();
        let use_arg = node.named_children(&mut node.walk()).find(|child| {
            matches!(
                child.kind(),
                "scoped_use_list" | "scoped_identifier" | "use_list" | "identifier"
            )
        })?;
        Some(ImportInfo {
            module_name: root_module(use_arg, source),
            signature: import_text,
            handled_refs: false,
        })
    }
}

fn root_module(node: Node<'_>, source: &str) -> String {
    let Some(first) = node.named_child(0) else {
        return node_text(node, source);
    };
    if matches!(first.kind(), "identifier" | "crate" | "super" | "self") {
        return node_text(first, source);
    }
    if first.kind() == "scoped_identifier" {
        return root_module(first, source);
    }
    node_text(first, source)
}

fn has_child_kind_recursive(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count()).any(|i| {
        node.child(i as u32)
            .is_some_and(|child| child.kind() == kind || has_child_kind_recursive(child, kind))
    })
}

fn is_identifier(text: &str) -> bool {
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
            .set_language(&tree_sitter_rust::LANGUAGE.into())
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
        assert_eq!(RUST_SPEC.name_field(), "name");
        assert_eq!(RUST_SPEC.body_field(), "body");
        assert_eq!(RUST_SPEC.params_field(), "parameters");
        assert_eq!(RUST_SPEC.return_field(), "return_type");
    }

    #[test]
    fn non_identifier_return_drops_to_none() {
        let src = "fn f() -> (i32, i32) { (0, 0) }\n";
        let tree = parse(src);
        let func = first_of_kind(tree.root_node(), "function_item").unwrap();
        assert!(RUST_SPEC.get_return_type(func, src).is_none());
    }

    #[test]
    fn private_and_default_visibility() {
        let src = "fn hidden() {}\n";
        let tree = parse(src);
        let func = first_of_kind(tree.root_node(), "function_item").unwrap();
        assert_eq!(RUST_SPEC.get_visibility(func).as_deref(), Some("private"));
    }

    #[test]
    fn generic_impl_receiver_via_generic_type() {
        let src = "struct S<T>(T);\nimpl<T> S<T> { fn m(&self) {} }\n";
        let tree = parse(src);
        let func = first_of_kind(tree.root_node(), "function_item").unwrap();
        assert_eq!(RUST_SPEC.get_receiver_type(func, src).as_deref(), Some("S"));
    }

    #[test]
    fn trait_impl_receivers_use_the_implementing_type_field() {
        for (src, expected) in [
            (
                "trait Tr { fn m(&self); }\nstruct G<T>(T);\nimpl<T> Tr for G<T> { fn m(&self) {} }\n",
                Some("G"),
            ),
            (
                "trait Tr { fn m(&self); }\nstruct P<'a>(&'a u8);\nimpl<'a> Tr for P<'a> { fn m(&self) {} }\n",
                Some("P"),
            ),
            (
                "trait Tr { fn m(&self); }\nstruct S;\nimpl Tr for &S { fn m(&self) {} }\n",
                Some("S"),
            ),
            (
                "trait Tr { fn m(&self); }\nmod m { pub struct S; }\nimpl Tr for m::S { fn m(&self) {} }\n",
                Some("S"),
            ),
            (
                "trait Tr { fn m(&self); }\nimpl Tr for (u8, u8) { fn m(&self) {} }\n",
                None,
            ),
            (
                "trait Tr { fn m(&self); }\nimpl Tr for dyn Tr { fn m(&self) {} }\n",
                None,
            ),
            (
                "trait Tr { fn m(&self); }\nimpl Tr for *const u8 { fn m(&self) {} }\n",
                None,
            ),
            (
                "trait Tr { fn m(&self); }\nimpl Tr for u32 { fn m(&self) {} }\n",
                None,
            ),
        ] {
            let tree = parse(src);
            let funcs = {
                let mut out = Vec::new();
                fn walk<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
                    if node.kind() == "function_item" {
                        out.push(node);
                    }
                    for index in 0..node.named_child_count() {
                        if let Some(child) = node.named_child(index as u32) {
                            walk(child, out);
                        }
                    }
                }
                walk(tree.root_node(), &mut out);
                out
            };
            let implementation = *funcs.last().expect("impl method");
            assert_eq!(
                RUST_SPEC.get_receiver_type(implementation, src).as_deref(),
                expected,
                "{src}"
            );
        }
    }

    #[test]
    fn use_root_module_scoped_and_leaf_forms() {
        let src = "use a::b::c;\nuse solo;\n";
        let tree = parse(src);
        let mut uses = Vec::new();
        fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
            if n.kind() == "use_declaration" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        walk(tree.root_node(), &mut uses);
        let scoped = RUST_SPEC.extract_import(uses[0], src).unwrap();
        assert_eq!(scoped.module_name, "a");
        let leaf = RUST_SPEC.extract_import(uses[1], src).unwrap();
        assert_eq!(leaf.module_name, "solo");
    }
}
