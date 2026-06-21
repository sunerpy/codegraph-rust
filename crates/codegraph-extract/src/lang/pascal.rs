//! Pascal `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/pascal.ts:5-62`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct PascalSpec;

pub static PASCAL_SPEC: PascalSpec = PascalSpec;

impl LanguageSpec for PascalSpec {
    fn language(&self) -> Language {
        Language::Pascal
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_pascal::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["declProc"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["declClass"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["declProc"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &["declIntf"]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["declEnum"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["declType"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["declUses"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["exprCall"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["declField", "declConst"]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "body"
    }

    fn params_field(&self) -> &'static str {
        "args"
    }

    fn return_field(&self) -> &'static str {
        "type"
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let args = child_by_field(node, "args");
        let return_type = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "typeref");
        if args.is_none() && return_type.is_none() {
            return None;
        }
        let mut sig = args.map(|args| node_text(args, source)).unwrap_or_default();
        if let Some(return_type) = return_type {
            sig.push_str(": ");
            sig.push_str(&node_text(return_type, source));
        }
        (!sig.is_empty()).then_some(sig)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "declSection" {
                for i in 0..parent.child_count() {
                    if let Some(child) = parent.child(i as u32) {
                        match child.kind() {
                            "kPublic" | "kPublished" => return Some("public".to_string()),
                            "kPrivate" => return Some("private".to_string()),
                            "kProtected" => return Some("protected".to_string()),
                            _ => {}
                        }
                    }
                }
            }
            current = parent.parent();
        }
        None
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        (0..node.child_count()).any(|i| node.child(i as u32).is_some_and(|c| c.kind() == "kClass"))
    }

    fn is_const(&self, node: Node<'_>) -> bool {
        node.kind() == "declConst"
    }

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        let name = child_by_field(node, "name").map(|name| node_text(name, source))?;
        name.rsplit_once('.').map(|(_, method)| method.to_string())
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let name = child_by_field(node, "name").map(|name| node_text(name, source))?;
        name.rsplit_once('.')
            .map(|(receiver, _)| receiver.to_string())
    }

    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        for child in node.named_children(&mut node.walk()) {
            match child.kind() {
                "declClass" => return Some(NodeKind::Class),
                "declIntf" => return Some(NodeKind::Interface),
                "declEnum" => return Some(NodeKind::Enum),
                _ => {}
            }
        }
        None
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = node_text(node, source).trim().to_string();
        let mut modules = Vec::new();
        for child in node.named_children(&mut node.walk()) {
            if matches!(child.kind(), "identifier" | "id" | "moduleName") {
                if child.kind() == "moduleName" {
                    if let Some(identifier) = child
                        .named_children(&mut child.walk())
                        .find(|inner| matches!(inner.kind(), "identifier" | "id"))
                    {
                        modules.push(node_text(identifier, source));
                    }
                } else {
                    modules.push(node_text(child, source));
                }
            }
        }
        let module_name = modules.join(",");
        (!module_name.is_empty()).then_some(ImportInfo {
            module_name,
            signature,
            handled_refs: false,
        })
    }
}
