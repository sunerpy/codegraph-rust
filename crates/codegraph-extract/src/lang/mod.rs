//! Per-language [`LanguageSpec`](crate::spec::LanguageSpec) implementations.
//!
//! Wave 1c intentionally wires only TypeScript. The module boundary is the
//! mechanical porting seam for tasks 14-17.

mod arkts;
mod c;
mod cfml;
mod cpp;
mod csharp;
mod dart;
mod erlang;
mod gdscript;
mod go;
mod java;
mod javascript;
mod jsx;
mod kotlin;
mod lua;
mod luau;
mod nix;
mod objc;
mod pascal;
mod php;
mod python;
mod r;
mod ruby;
mod rust;
mod scala;
mod solidity;
mod swift;
mod terraform;
mod tsx;
mod typescript;

use codegraph_core::types::Language;

use crate::spec::LanguageSpec;

pub use arkts::ARKTS_SPEC;
pub use c::C_SPEC;
pub use cfml::CFML_SPEC;
pub(crate) use cfml::{
    cfml_component_name_from_path, cfml_string_attr_value, cfml_tag_attr, is_bare_script_cfml,
};
pub use cpp::CPP_SPEC;
pub(crate) use cpp::{ExportMacroClass, detect_export_macro_class};
pub use csharp::CSHARP_SPEC;
pub use dart::DART_SPEC;
pub use erlang::ERLANG_SPEC;
pub(crate) use erlang::{
    erlang_atom_text, erlang_call_ref_name, erlang_clause_header, erlang_clause_name,
    erlang_collapse_ws, erlang_fun_value_ref_name, erlang_function_clauses, erlang_macro_name,
    erlang_module_exports, erlang_preceding_spec, erlang_record_field_name, erlang_record_ref_name,
    erlang_type_alias_name,
};
pub use gdscript::GDSCRIPT_SPEC;
pub use go::GO_SPEC;
pub use java::JAVA_SPEC;
pub use javascript::JAVASCRIPT_SPEC;
pub use jsx::JSX_SPEC;
pub use kotlin::KOTLIN_SPEC;
pub use lua::LUA_SPEC;
pub use luau::LUAU_SPEC;
pub use nix::NIX_SPEC;
pub(crate) use nix::{
    format_function_signature, is_callpackage_name, is_returned_attrset_member,
    is_static_project_path, nix_callee_name, nix_curried_params_and_body, nix_direct_callee_name,
    nix_first_apply_argument, nix_inherited_attrs, nix_static_import_path,
};
pub use objc::OBJC_SPEC;
pub use pascal::PASCAL_SPEC;
pub use php::PHP_SPEC;
pub use python::PYTHON_SPEC;
pub use r::R_SPEC;
pub use ruby::RUBY_SPEC;
pub use rust::RUST_SPEC;
pub use scala::SCALA_SPEC;
pub use solidity::SOLIDITY_SPEC;
pub use swift::SWIFT_SPEC;
pub use terraform::TERRAFORM_SPEC;
pub(crate) use terraform::{
    TerraformBlockDecl, collect_terraform_references, describe_terraform_block,
    read_terraform_block_header, terraform_block_body,
};
pub use tsx::TSX_SPEC;
pub use typescript::TYPESCRIPT_SPEC;

pub fn spec_for_language(language: Language) -> Option<&'static dyn LanguageSpec> {
    match language {
        Language::TypeScript => Some(&TYPESCRIPT_SPEC),
        Language::Tsx => Some(&TSX_SPEC),
        Language::JavaScript => Some(&JAVASCRIPT_SPEC),
        Language::Jsx => Some(&JSX_SPEC),
        Language::ArkTs => Some(&ARKTS_SPEC),
        Language::Python => Some(&PYTHON_SPEC),
        Language::Go => Some(&GO_SPEC),
        Language::Rust => Some(&RUST_SPEC),
        Language::Java => Some(&JAVA_SPEC),
        Language::C => Some(&C_SPEC),
        Language::Cpp => Some(&CPP_SPEC),
        Language::CSharp => Some(&CSHARP_SPEC),
        Language::Ruby => Some(&RUBY_SPEC),
        Language::Php => Some(&PHP_SPEC),
        Language::Scala => Some(&SCALA_SPEC),
        Language::Lua => Some(&LUA_SPEC),
        Language::Luau => Some(&LUAU_SPEC),
        Language::Dart => Some(&DART_SPEC),
        Language::Kotlin => Some(&KOTLIN_SPEC),
        Language::Swift => Some(&SWIFT_SPEC),
        Language::Pascal => Some(&PASCAL_SPEC),
        Language::ObjC => Some(&OBJC_SPEC),
        Language::R => Some(&R_SPEC),
        Language::Solidity => Some(&SOLIDITY_SPEC),
        Language::Nix => Some(&NIX_SPEC),
        Language::Terraform => Some(&TERRAFORM_SPEC),
        Language::Erlang => Some(&ERLANG_SPEC),
        Language::Cfml => Some(&CFML_SPEC),
        Language::Gdscript => Some(&GDSCRIPT_SPEC),
        _ => None,
    }
}
