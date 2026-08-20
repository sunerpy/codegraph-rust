//! codegraph-core crate

pub mod config;
pub mod errors;
pub mod file_class;
pub mod generated_header;
pub mod index_paths;
pub mod logger;
pub mod node_id;
pub mod traits;
pub mod types;

pub use errors::{CodeGraphError, Result};
pub use index_paths::{IndexPaths, IndexPathsError};
