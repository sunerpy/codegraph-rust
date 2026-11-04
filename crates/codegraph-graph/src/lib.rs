//! codegraph-graph crate
pub mod branch_guards;
pub mod centrality;
pub mod dead_code;
pub mod export;
pub mod flow_boundary;
pub mod graph;
pub mod hierarchy;
pub mod named_symbol_flow;
pub mod query;
pub mod segment_match;
pub mod segments;
pub mod symbol_lookup;
pub mod synthesized_hop;

/// Moved to [`codegraph_core::collation`], which the resolver's passes share.
pub use codegraph_core::collation;
