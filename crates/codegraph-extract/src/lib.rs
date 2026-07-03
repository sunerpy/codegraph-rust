//! codegraph-extract crate

#![allow(
    clippy::collapsible_if,
    clippy::collapsible_str_replace,
    clippy::filter_next,
    clippy::if_same_then_else,
    clippy::manual_contains
)]

pub mod embedded;
pub mod engine;
pub mod ext_config;
pub mod function_ref;
pub mod lang;
pub mod spec;
pub mod walker;

pub use engine::{ExtractOptions, detect_language, extract_file, extract_project, extract_source};
