//! Framework-resolver EXTENSION POINT.
//!
//! This module defines the [`FrameworkResolver`] trait — a documented extension
//! point porting the upstream `FrameworkResolver` interface
//! (`upstream resolution/types.ts:157-197`).
//!
//! # Concrete implementations
//!
//! The upstream ships ~22 heuristic `FrameworkResolver` implementations. The v1 Rust
//! port implements THREE of them — React/Next.js, Vue/Nuxt, and NestJS — in
//! [`crate::frameworks`], detected per-project by
//! [`crate::frameworks::detect_frameworks`]. The remaining ~19 (Spring/Java,
//! React-Native, Go-module, Drupal, Laravel, Express, Svelte, Astro, Python,
//! Ruby, C#, Swift, etc.) stay deferred (see `KNOWN_DIFFS.md`): they are an
//! additive heuristic layer on top of the deterministic import + name-matching
//! core, and porting them is tracked as future work.
//!
//! The detected implementations participate in
//! [`crate::resolver::ReferenceResolver::resolve_one`] exactly where the upstream's
//! `FrameworkResolver` strategy runs (`index.ts:695-701`); their `extract` /
//! `post_extract` passes run from the CLI index + sync flow mirroring the upstream's
//! `tree-sitter.ts` framework-extraction pass and `index.ts` `runPostExtract`.
//! On a project where none are detected the orchestrator holds an empty list, so
//! behavior is identical to the upstream with zero `FrameworkResolver`s detected.

use crate::types::{FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedRef};
use codegraph_core::types::Language;

/// A framework-specific resolution strategy (EXTENSION POINT).
///
/// Ports the upstream `FrameworkResolver` interface (`types.ts:157-197`). Every
/// method maps 1:1 to an upstream field; `Option`-returning methods model the upstream
/// `?:` optionals. Three implementations ship in v1 ([`crate::frameworks`]:
/// React/Next.js, Vue/Nuxt, NestJS); the trait remains the seam through which
/// the remaining upstream resolvers (Java/React-Native/Go-module/Swift-ObjC/…) can
/// be added later.
pub trait FrameworkResolver: Sync {
    /// `FrameworkResolver` name (`name`, `types.ts:159`).
    fn name(&self) -> &str;

    /// Languages this resolver applies to; `None` = all languages
    /// (`languages?`, `types.ts:161`).
    fn languages(&self) -> Option<&[Language]> {
        None
    }

    /// Detect whether the project uses this `FrameworkResolver`'s target — called
    /// once at startup (`detect`, `types.ts:163`).
    fn detect(&self, context: &dyn ResolutionContext) -> bool;

    /// Resolve a reference using this `FrameworkResolver`'s convention patterns
    /// (`resolve`, `types.ts:165`).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef>;

    /// Opt a reference NAME through the name-exists pre-filter even when no node
    /// is named that — needed for dynamic dispatch (`claimsReference?`,
    /// `types.ts:166-173`). Defaults to `false`.
    fn claims_reference(&self, _name: &str) -> bool {
        false
    }

    /// Extract this `FrameworkResolver`'s nodes + references from a file
    /// (`extract?`, `types.ts:174-182`). `None` when the resolver has no
    /// per-file extraction.
    ///
    /// `file_path` is the repo-RELATIVE path that MUST be used for all node /
    /// reference attribution (preserving golden byte-stability). `project_root`
    /// is the absolute project root the pipeline is indexing; resolvers that
    /// read a per-project `.codegraph/codegraph.json` (e.g. Godot's opt-in DSL
    /// config) MUST resolve that config against `project_root.join(file_path)`
    /// rather than the process CWD — otherwise the config is only found when the
    /// CLI happens to run with its CWD == the project root. Resolvers that need
    /// no project config simply ignore `project_root`.
    fn extract(
        &self,
        _file_path: &str,
        _content: &str,
        _project_root: &str,
    ) -> Option<FrameworkResolverExtractionResult> {
        None
    }

    /// Cross-file finalization pass run once after all extraction completes
    /// (`postExtract?`, `types.ts:183-196`). Returns nodes with mutated fields
    /// (node `id` preserved) for the orchestrator to persist. `None` when the
    /// resolver has no finalization pass.
    fn post_extract(
        &self,
        _context: &dyn ResolutionContext,
    ) -> Option<Vec<codegraph_core::types::Node>> {
        None
    }
}
