//! JSX `LanguageSpec` wrapper.
//!
//! Mirrors `upstream extraction/languages/index.ts:33-34`: JSX uses
//! the JavaScript extractor with the JavaScript grammar entry from
//! `upstream extraction/grammars.ts:22-23`.

use codegraph_core::types::Language;
use tree_sitter::Language as TsLanguage;

use crate::lang::javascript::JAVASCRIPT_SPEC;
use crate::spec::{ImportInfo, LanguageSpec};
use tree_sitter::Node;

pub struct JsxSpec;

pub static JSX_SPEC: JsxSpec = JsxSpec;

impl LanguageSpec for JsxSpec {
    fn language(&self) -> Language {
        Language::Jsx
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.function_types()
    }
    fn class_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.class_types()
    }
    fn method_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.method_types()
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
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.import_types()
    }
    fn call_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.call_types()
    }
    fn variable_types(&self) -> &'static [&'static str] {
        JAVASCRIPT_SPEC.variable_types()
    }
    fn name_field(&self) -> &'static str {
        JAVASCRIPT_SPEC.name_field()
    }
    fn body_field(&self) -> &'static str {
        JAVASCRIPT_SPEC.body_field()
    }
    fn params_field(&self) -> &'static str {
        JAVASCRIPT_SPEC.params_field()
    }
    fn return_field(&self) -> &'static str {
        JAVASCRIPT_SPEC.return_field()
    }
    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        JAVASCRIPT_SPEC.resolve_body(node, body_field)
    }
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        JAVASCRIPT_SPEC.get_signature(node, source)
    }
    fn is_exported(&self, node: Node<'_>, source: &str) -> bool {
        JAVASCRIPT_SPEC.is_exported(node, source)
    }
    fn is_async(&self, node: Node<'_>) -> bool {
        JAVASCRIPT_SPEC.is_async(node)
    }
    fn is_const(&self, node: Node<'_>) -> bool {
        JAVASCRIPT_SPEC.is_const(node)
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        JAVASCRIPT_SPEC.extract_import(node, source)
    }
}
