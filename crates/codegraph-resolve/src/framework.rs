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

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{EdgeKind, Node, NodeKind};

    struct EmptyContext;

    impl ResolutionContext for EmptyContext {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
            Vec::new()
        }
        fn file_exists(&self, _file_path: &str) -> bool {
            false
        }
        fn read_file(&self, _file_path: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_node_by_id(&self, _id: &str) -> Option<Node> {
            None
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    use crate::types::ImportMapping;

    /// Overrides ONLY the two required methods so every defaulted method
    /// (`languages`, `claims_reference`, `extract`, `post_extract`) runs its
    /// trait-default body under test.
    struct BareResolver;

    impl FrameworkResolver for BareResolver {
        fn name(&self) -> &str {
            "bare"
        }
        fn detect(&self, _context: &dyn ResolutionContext) -> bool {
            true
        }
        fn resolve(
            &self,
            _reference: &RefView,
            _context: &dyn ResolutionContext,
        ) -> Option<ResolvedRef> {
            None
        }
    }

    fn a_ref() -> RefView {
        RefView {
            from_node_id: "from".to_string(),
            reference_name: "X".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            column: 0,
            file_path: "a.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    #[test]
    fn bare_resolver_required_methods() {
        let ctx = EmptyContext;
        assert_eq!(BareResolver.name(), "bare");
        assert!(BareResolver.detect(&ctx));
        assert!(BareResolver.resolve(&a_ref(), &ctx).is_none());
    }

    #[test]
    fn languages_default_is_none() {
        // `languages()` default returns None (applies to all languages).
        assert!(BareResolver.languages().is_none());
    }

    #[test]
    fn claims_reference_default_is_false() {
        // The default `claims_reference` returns false for any name.
        assert!(!BareResolver.claims_reference("anything"));
        assert!(!BareResolver.claims_reference(""));
    }

    #[test]
    fn extract_default_is_none() {
        // The default `extract` returns None (no per-file extraction).
        assert!(BareResolver.extract("a.ts", "content", "/root").is_none());
    }

    #[test]
    fn post_extract_default_is_none() {
        // The default `post_extract` returns None (no finalization pass).
        let ctx = EmptyContext;
        assert!(BareResolver.post_extract(&ctx).is_none());
    }

    #[test]
    fn resolver_is_usable_as_trait_object() {
        // Boxing exercises the `Sync` bound + dynamic dispatch through the trait
        // object the orchestrator holds.
        let boxed: Box<dyn FrameworkResolver> = Box::new(BareResolver);
        assert_eq!(boxed.name(), "bare");
        assert!(boxed.languages().is_none());
    }

    #[test]
    fn empty_context_methods_return_absent_values() {
        // Drives every EmptyContext method so the helper stub is fully exercised.
        let c = EmptyContext;
        assert!(c.get_nodes_in_file("a").is_empty());
        assert!(c.get_nodes_by_name("a").is_empty());
        assert!(c.get_nodes_by_qualified_name("a").is_empty());
        assert!(c.get_nodes_by_kind(NodeKind::Function).is_empty());
        assert!(!c.file_exists("a"));
        assert!(c.read_file("a").is_none());
        assert_eq!(c.get_project_root(), "/project");
        assert!(c.get_all_files().is_empty());
        assert!(c.get_nodes_by_lower_name("a").is_empty());
        assert!(c.get_node_by_id("a").is_none());
        assert!(c.get_import_mappings("a", Language::TypeScript).is_empty());
    }
}
