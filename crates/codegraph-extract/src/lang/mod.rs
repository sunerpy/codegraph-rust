//! Per-language [`LanguageSpec`](crate::spec::LanguageSpec) implementations.
//!
//! Wave 1c intentionally wires only TypeScript. The module boundary is the
//! mechanical porting seam for tasks 14-17.

mod c;
mod cpp;
mod csharp;
mod dart;
mod gdscript;
mod go;
mod java;
mod javascript;
mod jsx;
mod kotlin;
mod lua;
mod luau;
mod objc;
mod pascal;
mod php;
mod python;
mod r;
mod ruby;
mod rust;
mod scala;
mod swift;
mod tsx;
mod typescript;

use codegraph_core::types::Language;

use crate::spec::LanguageSpec;

pub use c::C_SPEC;
pub use cpp::CPP_SPEC;
pub use csharp::CSHARP_SPEC;
pub use dart::DART_SPEC;
pub use gdscript::GDSCRIPT_SPEC;
pub use go::GO_SPEC;
pub use java::JAVA_SPEC;
pub use javascript::JAVASCRIPT_SPEC;
pub use jsx::JSX_SPEC;
pub use kotlin::KOTLIN_SPEC;
pub use lua::LUA_SPEC;
pub use luau::LUAU_SPEC;
pub use objc::OBJC_SPEC;
pub use pascal::PASCAL_SPEC;
pub use php::PHP_SPEC;
pub use python::PYTHON_SPEC;
pub use r::R_SPEC;
pub use ruby::RUBY_SPEC;
pub use rust::RUST_SPEC;
pub use scala::SCALA_SPEC;
pub use swift::SWIFT_SPEC;
pub use tsx::TSX_SPEC;
pub use typescript::TYPESCRIPT_SPEC;

pub fn spec_for_language(language: Language) -> Option<&'static dyn LanguageSpec> {
    match language {
        Language::TypeScript => Some(&TYPESCRIPT_SPEC),
        Language::Tsx => Some(&TSX_SPEC),
        Language::JavaScript => Some(&JAVASCRIPT_SPEC),
        Language::Jsx => Some(&JSX_SPEC),
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
        Language::Gdscript => Some(&GDSCRIPT_SPEC),
        _ => None,
    }
}
