//! codegraph-resolve — deterministic cross-file reference resolution.
//!
//! Ports the CORE deterministic strategies of `upstream resolution/`:
//! import resolution ([`import_resolver`]) and name matching ([`name_matcher`]),
//! orchestrated by [`resolver::ReferenceResolver`] over a
//! [`types::ResolutionContext`] (the store-backed one is [`context::StoreResolutionContext`]).
//! It consumes `unresolved_refs` from `codegraph-store` and writes resolved
//! edges back. Resolution is synchronous (rusqlite).
//!
//! The heuristic `FrameworkResolver` layer is a small additive layer on top of
//! the deterministic import + name-matching core. [`framework::FrameworkResolver`]
//! is the documented EXTENSION POINT; v1 ships THREE concrete implementations —
//! React/Next.js, Vue/Nuxt, and NestJS ([`frameworks`]) — detected per-project
//! by [`frameworks::detect_frameworks`]. The other ~19 upstream resolvers remain
//! deferred (see `KNOWN_DIFFS.md`).

#![allow(clippy::collapsible_if, clippy::collapsible_else_if)]

mod alias_binding;
mod awaited;
mod c_macro_visibility;
pub mod context;
pub mod framework; // the FrameworkResolver extension point
pub mod frameworks; // concrete react/vue/nestjs FrameworkResolvers
pub mod import_resolver;
mod js_store;
pub mod lru_cache;
mod member_value;
pub mod name_matcher;
mod object_literal;
pub mod path_aliases;
pub mod pathutil;
pub mod resolver;
pub mod snapshot_context;
pub mod source_facts;
pub mod strip_comments;
pub mod types;
pub mod workspace_packages;

pub use context::StoreResolutionContext;
pub use framework::FrameworkResolver;
pub use resolver::{ReferenceResolver, ResolutionObservation};
pub use snapshot_context::{EdgeAdjacency, SnapshotResolutionContext, build_edge_adjacency};
pub use types::{
    FrameworkResolverExtractionResult, ImportMapping, ReExport, RefView, ResolutionContext,
    ResolutionResult, ResolutionStats, ResolvedBy, ResolvedRef,
};
