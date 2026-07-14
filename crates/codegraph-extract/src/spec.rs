//! Language-specific extraction contracts.
//!
//! Mirrors the upstream `LanguageExtractor` interface from
//! `upstream extraction/tree-sitter-types.ts:73-254`. The generic
//! dispatcher in [`crate::walker`] consumes this trait the same way the upstream
//! `TreeSitterExtractor.visitNode` consumes `EXTRACTORS[language]` in
//! `tree-sitter.ts:355-578`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

#[derive(Debug, Clone)]
pub struct ImportInfo {
    pub module_name: String,
    pub signature: String,
    pub handled_refs: bool,
}

pub trait LanguageSpec: Sync {
    fn language(&self) -> Language;
    fn tree_sitter_language(&self) -> TsLanguage;

    fn function_types(&self) -> &'static [&'static str];
    fn class_types(&self) -> &'static [&'static str];
    fn method_types(&self) -> &'static [&'static str];
    fn interface_types(&self) -> &'static [&'static str];
    fn struct_types(&self) -> &'static [&'static str];
    fn enum_types(&self) -> &'static [&'static str];
    fn enum_member_types(&self) -> &'static [&'static str];
    fn type_alias_types(&self) -> &'static [&'static str];
    fn import_types(&self) -> &'static [&'static str];
    fn call_types(&self) -> &'static [&'static str];
    fn variable_types(&self) -> &'static [&'static str];

    fn field_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn property_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn module_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn package_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn extra_class_node_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn name_field(&self) -> &'static str;
    fn body_field(&self) -> &'static str;
    fn params_field(&self) -> &'static str;
    fn return_field(&self) -> &'static str;

    fn pre_parse(&self, source: &str, file_path: &str) -> String {
        let _ = file_path;
        source.to_string()
    }

    fn resolve_body<'tree>(&self, _node: Node<'tree>, _body_field: &str) -> Option<Node<'tree>> {
        None
    }

    fn resolve_name(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn classify_class_node(&self, _node: Node<'_>) -> NodeKind {
        NodeKind::Class
    }

    fn interface_kind(&self) -> NodeKind {
        NodeKind::Interface
    }

    fn methods_are_top_level(&self) -> bool {
        false
    }

    fn get_signature(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn get_return_type(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn get_visibility(&self, _node: Node<'_>) -> Option<String> {
        None
    }

    fn is_exported(&self, _node: Node<'_>, _source: &str) -> bool {
        false
    }

    fn is_async(&self, _node: Node<'_>) -> bool {
        false
    }

    fn is_static(&self, _node: Node<'_>, _source: &str) -> bool {
        false
    }

    fn is_const(&self, _node: Node<'_>) -> bool {
        false
    }

    fn is_misparsed_function(&self, _name: &str, _node: Node<'_>, _source: &str) -> bool {
        false
    }

    fn get_receiver_type(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn extract_property_name(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Mirrors the upstream `classifyTsClassMember` (typescript.ts:16-38): a class field
    /// is a method only when its initializer is callable; default true.
    fn class_member_is_method(&self, _node: Node<'_>, _source: &str) -> bool {
        true
    }

    fn resolve_type_alias_kind(&self, _node: Node<'_>, _source: &str) -> Option<NodeKind> {
        None
    }

    fn extract_package(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn extract_bare_call(&self, _node: Node<'_>, _source: &str) -> Option<String> {
        None
    }

    fn extract_import(&self, _node: Node<'_>, _source: &str) -> Option<ImportInfo> {
        None
    }

    /// Extra symbol-level modifiers merged into the node's decorators list.
    /// Mirrors the upstream `extractModifiers` hook (`tree-sitter.ts:626-634`),
    /// used by Kotlin for `expect`/`actual` markers (kotlin.ts:271-293).
    fn extract_modifiers(&self, _node: Node<'_>) -> Vec<String> {
        Vec::new()
    }
}

pub(crate) fn has_type(types: &[&str], node_type: &str) -> bool {
    types.contains(&node_type)
}
