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

pub use engine::{
    ExtractOptions, ExtractionStage, PARSE_COLLAPSE_WARNING, RootGitignore, detect_language,
    detect_language_with, extract_file, extract_file_with_options,
    extract_file_with_options_observer, extract_project, extract_source, extract_source_with,
    extract_source_with_observer, include_exclude_pattern_matches, is_extraction_warning,
};
pub use ext_config::ExtensionOverrides;
