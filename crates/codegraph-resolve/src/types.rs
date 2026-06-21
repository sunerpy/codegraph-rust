//! Reference-resolution types.
//!
//! Ports `upstream resolution/types.ts`. The upstream `UnresolvedRef`
//! interface (types.ts:12-29) is the denormalized, in-memory resolution shape
//! (it always carries `filePath`/`language`), distinct from the persisted
//! [`codegraph_core::types::UnresolvedRef`] row read from the store. We model it
//! here as [`RefView`] so the two never get confused.

use codegraph_core::types::{EdgeKind, Language, Node};

/// An unresolved reference in resolution-ready form.
///
/// Ports the upstream `UnresolvedRef` interface (`types.ts:12-29`). Unlike the
/// stored [`codegraph_core::types::UnresolvedRef`], `file_path` and `language`
/// are always populated (the orchestrator denormalizes them from the source
/// node when the row omitted them — `index.ts:522-531`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefView {
    /// ID of the source node containing the reference (`fromNodeId`).
    pub from_node_id: String,
    /// The name being referenced (`referenceName`).
    pub reference_name: String,
    /// Type of reference (`referenceKind`).
    pub reference_kind: EdgeKind,
    /// Line where the reference occurs.
    pub line: i64,
    /// Column where the reference occurs.
    pub column: i64,
    /// File path where the reference occurs.
    pub file_path: String,
    /// Language of the source file.
    pub language: Language,
    /// Mirrors [`codegraph_core::types::UnresolvedRef::is_function_ref`] — a
    /// upstream `function_ref` callback registration that resolves to a
    /// `references` edge tagged `fnRef: true`.
    pub is_function_ref: bool,
}

/// How a reference was resolved.
///
/// Ports the `ResolvedRef['resolvedBy']` union (`types.ts:42`). The exact
/// strings feed `edges.metadata.resolvedBy`, so [`ResolvedBy::as_str`] must
/// match the upstream byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBy {
    ExactMatch,
    Import,
    QualifiedName,
    Framework, // produced by a FrameworkResolver (react/vue/nestjs in v1)
    Fuzzy,
    InstanceMethod,
    FilePath,
    FunctionRef,
}

impl ResolvedBy {
    /// The upstream `resolvedBy` string written to `edges.metadata`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactMatch => "exact-match",
            Self::Import => "import",
            Self::QualifiedName => "qualified-name",
            Self::Framework => "framework", // FrameworkResolver extension-point output
            Self::Fuzzy => "fuzzy",
            Self::InstanceMethod => "instance-method",
            Self::FilePath => "file-path",
            Self::FunctionRef => "function-ref",
        }
    }
}

/// A resolved reference.
///
/// Ports the upstream `ResolvedRef` interface (`types.ts:34-43`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRef {
    /// Original unresolved reference.
    pub original: RefView,
    /// ID of the target node.
    pub target_node_id: String,
    /// Confidence score (0-1).
    pub confidence: f64,
    /// How it was resolved.
    pub resolved_by: ResolvedBy,
}

/// Aggregate resolution statistics.
///
/// Ports `ResolutionResult.stats` (`types.ts:53-59`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionStats {
    pub total: usize,
    pub resolved: usize,
    pub unresolved: usize,
    /// `byMethod`: count of resolutions keyed by `resolvedBy` string.
    pub by_method: std::collections::BTreeMap<String, usize>,
}

/// Result of a resolution pass.
///
/// Ports the upstream `ResolutionResult` interface (`types.ts:48-60`).
#[derive(Debug, Clone, Default)]
pub struct ResolutionResult {
    pub resolved: Vec<ResolvedRef>,
    pub unresolved: Vec<RefView>,
    pub stats: ResolutionStats,
}

/// An import mapping extracted from a file.
///
/// Ports the upstream `ImportMapping` interface (`types.ts:202-215`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMapping {
    /// Local name used in the file.
    pub local_name: String,
    /// Original exported name (may differ due to aliasing).
    pub exported_name: String,
    /// Source module/path.
    pub source: String,
    /// Whether it's a default import.
    pub is_default: bool,
    /// Whether it's a namespace import (`import * as X`).
    pub is_namespace: bool,
}

/// A re-export declared by a file.
///
/// Ports the upstream `ReExport` union (`types.ts:222-235`):
/// `export { x } from './other'` (named) or `export * from './other'`
/// (wildcard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReExport {
    /// `export { originalName as exportedName } from 'source'`.
    Named {
        /// Name as exported by THIS file.
        exported_name: String,
        /// Name in the upstream module (differs when renamed via `as`).
        original_name: String,
        /// Module specifier of the upstream module.
        source: String,
    },
    /// `export * from 'source'`.
    Wildcard {
        /// Module specifier of the upstream module.
        source: String,
    },
}

/// Result of a [`FrameworkResolver`] extension's file extraction.
///
/// Ports the upstream `FrameworkResolverExtractionResult` interface (`types.ts:147-152`).
/// Produced by the react/vue/nestjs [`FrameworkResolver`] `extract` passes.
///
/// [`FrameworkResolver`]: crate::framework::FrameworkResolver
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameworkResolverExtractionResult {
    /// `FrameworkResolver`-produced nodes (e.g. routes).
    pub nodes: Vec<Node>,
    /// `FrameworkResolver`-produced unresolved references (e.g. route → handler).
    pub references: Vec<RefView>,
}

/// Context for resolution — provides read access to the graph.
///
/// Ports the upstream `ResolutionContext` interface (`types.ts:65-142`). Methods
/// that are `optional?` in the upstream are modeled either as defaulted trait methods
/// (returning the "absent" value) or always-present methods, so concrete
/// implementations only override what they support. Resolution is synchronous
/// (rusqlite) so every method is sync.
pub trait ResolutionContext {
    /// Get all nodes in a file (`getNodesInFile`, ordered by `start_line`).
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node>;
    /// Get all nodes by name (`getNodesByName`).
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node>;
    /// Get all nodes by exact qualified name (`getNodesByQualifiedName`).
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node>;
    /// Get all nodes of a kind (`getNodesByKind`).
    fn get_nodes_by_kind(&self, kind: codegraph_core::types::NodeKind) -> Vec<Node>;

    /// All node names for the `warmCaches` known-name set (`index.ts:298-308`).
    /// Default loops every kind (old behavior); store contexts override to pull
    /// only the `name` column. Both feed a `BTreeSet`, so the set is identical.
    fn known_node_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for kind in codegraph_core::types::NodeKind::ALL {
            for node in self.get_nodes_by_kind(kind) {
                names.push(node.name);
            }
        }
        names
    }
    /// Check if a file exists (`fileExists`).
    fn file_exists(&self, file_path: &str) -> bool;
    /// Read file content (`readFile`); `None` when unreadable.
    fn read_file(&self, file_path: &str) -> Option<String>;
    /// Get project root (`getProjectRoot`).
    fn get_project_root(&self) -> &str;
    /// Get all files (`getAllFiles`).
    fn get_all_files(&self) -> Vec<String>;
    /// Get nodes by lowercase name (`getNodesByLowerName`, O(1) via index).
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node>;

    /// Resolve a node by id (`queries.getNodeById`). The upstream context reaches
    /// `queries` directly for this (`index.ts:1087,1094`); the Rust port exposes
    /// it on the trait so the orchestrator's language gates stay pure.
    fn get_node_by_id(&self, id: &str) -> Option<Node>;

    /// Direct supertypes of `type_name` in `language` (`getSupertypes?`).
    ///
    /// Backed by resolved `implements`/`extends` edges, so EMPTY during the
    /// first pass and populated afterward (`types.ts:84-93`). Defaults to empty
    /// so contexts without edge access compile unchanged.
    fn get_supertypes(&self, _type_name: &str, _language: Language) -> Vec<String> {
        Vec::new()
    }

    /// Cached import mappings for a file (`getImportMappings`).
    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping>;

    /// Project tsconfig/jsconfig path aliases (`getProjectAliases?`). `None`
    /// when the project declares none (`types.ts:96-103`).
    fn get_project_aliases(&self) -> Option<crate::path_aliases::AliasMap> {
        None
    }

    /// Monorepo workspace member packages (`getWorkspacePackages?`). `None` for
    /// single-package repos (`types.ts:113-118`).
    fn get_workspace_packages(&self) -> Option<crate::workspace_packages::WorkspacePackages> {
        None
    }

    /// Go module info from `go.mod` (`getGoModule?`). `None` for non-Go /
    /// module-less projects (`types.ts:104-111`).
    fn get_go_module(&self) -> Option<GoModule> {
        None
    }

    /// Re-exports declared by a file (`getReExports?`, `types.ts:119-125`).
    /// Empty when the file has none.
    fn get_re_exports(&self, _file_path: &str, _language: Language) -> Vec<ReExport> {
        Vec::new()
    }

    /// C/C++ include search directories (`getCppIncludeDirs?`,
    /// `types.ts:135-141`). Empty by default.
    fn get_cpp_include_dirs(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Go module info parsed from `go.mod`.
///
/// Mirrors the upstream `GoModule` (`upstream resolution/go-module.ts`),
/// referenced by `ResolutionContext.getGoModule?` (`types.ts:104-111`). Only
/// `module_path` is load-bearing for import classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoModule {
    pub module_path: String,
}
