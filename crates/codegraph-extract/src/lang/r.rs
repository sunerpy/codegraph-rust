//! R `LanguageSpec`.
//!
//! Ports `upstream extraction/languages/r.ts` (upstream 06a410e9 /
//! #828). R has no standard function/class/import node types — named functions
//! are `name <- function(...)` assignments, classes are `setClass`/`setRefClass`/
//! `R6Class`/`ggproto` calls, generics are `setGeneric`/`setMethod` calls, and
//! imports are `library()`/`require()`/`source()` calls. All real extraction is
//! driven by [`crate::walker::TreeSitterWalker::visit_r_node`]; this spec only
//! declares `call_types` so calls are dispatched, plus the field names.

use codegraph_core::types::Language;
use tree_sitter::Language as TsLanguage;

use crate::spec::LanguageSpec;

pub struct RSpec;

pub static R_SPEC: RSpec = RSpec;

impl LanguageSpec for RSpec {
    fn language(&self) -> Language {
        Language::R
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_r::LANGUAGE.into()
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
        &["call"]
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
        "type"
    }
}
