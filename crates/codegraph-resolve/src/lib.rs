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
mod class_heads;
pub mod context;
mod cpp_includers;
pub mod cpp_namespaces;
mod cpp_supertypes;
mod cpp_type_aliases;
mod csharp_extensions;
mod csharp_scopes;
pub mod destructured_calls;
pub mod framework; // the FrameworkResolver extension point
pub mod frameworks; // concrete react/vue/nestjs FrameworkResolvers
pub mod import_resolver;
pub mod java_scope;
mod js_store;
mod jsx_tags;
pub mod kotlin_scope;
pub mod lru_cache;
mod member_value;
pub mod module_surface;
pub mod name_matcher;
mod object_literal;
mod object_owners;
mod overloads;
pub mod package_manifests;
pub mod path_aliases;
pub mod pathutil;
pub mod python_globals;
mod python_values;
pub mod resolver;
mod rust_lexical;
pub mod sfc_scripts;
pub mod snapshot_context;
pub mod source_facts;
pub mod strip_comments;
mod type_parameters;
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
