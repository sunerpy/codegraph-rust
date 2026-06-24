//! GDScript (`.gd`) `LanguageSpec`.
//!
//! Non-upstream Rust-side addition. T2 wires a minimal compiling stub; symbol
//! extraction (functions, classes, enums, variables, extends/preload edges) is
//! filled in by later todos. All `*_types()` return `&[]` for now.

use codegraph_core::types::Language;
use tree_sitter::Language as TsLanguage;

use crate::spec::LanguageSpec;

pub struct GdscriptSpec;

pub static GDSCRIPT_SPEC: GdscriptSpec = GdscriptSpec;

impl LanguageSpec for GdscriptSpec {
    fn language(&self) -> Language {
        Language::Gdscript
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_gdscript::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &[]
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
        &[]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &[]
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
}
