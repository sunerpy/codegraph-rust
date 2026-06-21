use crate::errors::Result;
use crate::types::{Edge, ExtractionResult, FileRecord, Node, UnresolvedRef};

pub trait Extractor {
    fn extract(&self, file: &FileRecord) -> Result<ExtractionResult>;
}

/// Minimal test-mocking seam for crates that consume persisted graph data.
///
/// This is intentionally **not** a multi-backend storage abstraction: the real
/// store crate owns SQLite behavior. Keep this surface tiny and add methods only
/// when downstream crates need a mockable read/write seam.
pub trait Store {
    fn upsert_file(&mut self, file: &FileRecord) -> Result<()>;
    fn upsert_nodes(&mut self, nodes: &[Node]) -> Result<()>;
    fn upsert_edges(&mut self, edges: &[Edge]) -> Result<()>;
    fn add_unresolved_refs(&mut self, refs: &[UnresolvedRef]) -> Result<()>;
}
