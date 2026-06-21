//! TSX `LanguageSpec` wrapper.
//!
//! Mirrors `upstream extraction/languages/index.ts:31-32`: TSX uses
//! the TypeScript extractor with the TSX grammar.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::lang::typescript::TYPESCRIPT_SPEC;
use crate::spec::{ImportInfo, LanguageSpec};

pub struct TsxSpec;

pub static TSX_SPEC: TsxSpec = TsxSpec;

impl LanguageSpec for TsxSpec {
    fn language(&self) -> Language {
        Language::Tsx
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.function_types()
    }
    fn class_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.class_types()
    }
    fn method_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.method_types()
    }
    fn interface_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.interface_types()
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.enum_types()
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.enum_member_types()
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.type_alias_types()
    }
    fn import_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.import_types()
    }
    fn call_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.call_types()
    }
    fn variable_types(&self) -> &'static [&'static str] {
        TYPESCRIPT_SPEC.variable_types()
    }
    fn name_field(&self) -> &'static str {
        TYPESCRIPT_SPEC.name_field()
    }
    fn body_field(&self) -> &'static str {
        TYPESCRIPT_SPEC.body_field()
    }
    fn params_field(&self) -> &'static str {
        TYPESCRIPT_SPEC.params_field()
    }
    fn return_field(&self) -> &'static str {
        TYPESCRIPT_SPEC.return_field()
    }
    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        TYPESCRIPT_SPEC.resolve_body(node, body_field)
    }
    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        TYPESCRIPT_SPEC.classify_class_node(node)
    }
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        TYPESCRIPT_SPEC.get_signature(node, source)
    }
    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        TYPESCRIPT_SPEC.get_visibility(node)
    }
    fn is_exported(&self, node: Node<'_>, source: &str) -> bool {
        TYPESCRIPT_SPEC.is_exported(node, source)
    }
    fn is_async(&self, node: Node<'_>) -> bool {
        TYPESCRIPT_SPEC.is_async(node)
    }
    fn is_static(&self, node: Node<'_>, source: &str) -> bool {
        TYPESCRIPT_SPEC.is_static(node, source)
    }
    fn is_const(&self, node: Node<'_>) -> bool {
        TYPESCRIPT_SPEC.is_const(node)
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        TYPESCRIPT_SPEC.extract_import(node, source)
    }
}
