//! Reference-resolution types.
//!
//! Ports `upstream resolution/types.ts`. The upstream `UnresolvedRef`
//! interface (types.ts:12-29) is the denormalized, in-memory resolution shape
//! (it always carries `filePath`/`language`), distinct from the persisted
//! [`codegraph_core::types::UnresolvedRef`] row read from the store. We model it
//! here as [`RefView`] so the two never get confused.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, ReferenceSubkind};

/// The ES-module family: the languages where a symbol declared in another file is
/// visible ONLY through an explicit `import`, and the set
/// [`crate::import_resolver::extract_import_mappings`] serves through
/// `extract_js_imports`.
///
/// That equivalence is what makes "declared in this file, or imported into it" a
/// sound visibility oracle here and nowhere else: a package- or namespace-scoped
/// language (Go, Java, Kotlin, C#, Rust, Scala, Dart) makes a sibling-file type
/// visible with no import at all, so the same predicate would delete real edges
/// there.
pub const ESM_LANGUAGES: [Language; 6] = [
    Language::TypeScript,
    Language::Tsx,
    Language::JavaScript,
    Language::Jsx,
    Language::Svelte,
    Language::Vue,
];

/// Is `language` in the [`ESM_LANGUAGES`] family?
pub fn is_esm_language(language: Language) -> bool {
    ESM_LANGUAGES.contains(&language)
}

/// Node kinds that DECLARE a name usable as a type.
///
/// Two readings, deliberately one list:
/// - the only legal targets of an `extends`/`implements` reference — a
///   `property`, `enum_member` or `method` sharing a supertype's name is not a
///   supertype (#1537/#1536); and
/// - the same-file declarations that BIND a type name at a call site, which is
///   what the receiver-type visibility gate reads (#1566). A TS
///   `namespace Foo {}` binds `Foo` as much as a `class Foo` does, which is why
///   `Module`/`Namespace` belong here too.
///
/// Matches upstream's 11-kind set: `Union` joined it when `NodeKind::Union` was
/// added (PR #1516), because a C/C++/ObjC/Rust `union` declares a type name
/// exactly as a `struct` does.
pub const TYPE_DECLARING_KINDS: [NodeKind; 11] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
    NodeKind::TypeAlias,
    NodeKind::Component,
    NodeKind::Module,
    NodeKind::Namespace,
];

/// Does `kind` declare a name usable as a type ([`TYPE_DECLARING_KINDS`])?
pub fn declares_type_name(kind: NodeKind) -> bool {
    TYPE_DECLARING_KINDS.contains(&kind)
}

/// Node kinds an `imports` reference may NEVER target.
///
/// A member of a type is not importable on its own, so an `imports` reference
/// landing on one is fabricated: `import * as path from 'node:path'` binding to
/// some class's `path` property (#1537). Stated as a DENY list rather than an
/// allow list because the importable set is open-ended — a file, class, function,
/// constant, variable, type alias and enum are all legitimately importable — while
/// the members that are not form a small, closed set.
pub const NON_IMPORTABLE_KINDS: [NodeKind; 5] = [
    NodeKind::Property,
    NodeKind::Field,
    NodeKind::Method,
    NodeKind::EnumMember,
    NodeKind::Parameter,
];

/// Is `kind` a legal target for an `imports` reference?
pub fn is_importable_kind(kind: NodeKind) -> bool {
    !NON_IMPORTABLE_KINDS.contains(&kind)
}

/// Is `kind` an inheritance reference, i.e. one whose target must be a type
/// ([`TYPE_DECLARING_KINDS`])?
///
/// Judged on the reference kind as EXTRACTED. `create_edges` later promotes
/// `Extends` → `Implements` when the target is an interface/protocol, so an
/// `Extends` reference must be tested against the supertype set BEFORE that
/// promotion, not after.
pub fn is_inheritance_ref(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::Extends | EdgeKind::Implements)
}

/// May a reference of kind `reference_kind` resolve to a node of kind
/// `target_kind`? (#1537/#1536)
///
/// The two gates of item 6's kind eligibility, in one predicate so the candidate
/// filter and the resolution seam cannot drift apart. Every other reference kind
/// is unconstrained here and resolves exactly as before.
pub fn kind_is_eligible_target(reference_kind: EdgeKind, target_kind: NodeKind) -> bool {
    if is_inheritance_ref(reference_kind) {
        return declares_type_name(target_kind);
    }
    if reference_kind == EdgeKind::Imports {
        return is_importable_kind(target_kind);
    }
    true
}

/// An unresolved reference in resolution-ready form.
///
/// Ports the upstream `UnresolvedRef` interface (`types.ts:12-29`). Unlike the
/// stored [`codegraph_core::types::UnresolvedRef`], `file_path` and `language`
/// are always populated (the orchestrator denormalizes them from the source
/// node when the row omitted them — `index.ts:522-531`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefView {
    /// `unresolved_refs.id` of the PERSISTED row this view was read from
    /// (`INTEGER PRIMARY KEY AUTOINCREMENT`, so unique per row), threaded through
    /// resolution so cleanup can delete EXACTLY the row that resolved (#1269/#1270,
    /// upstream `e871c49`).
    ///
    /// `None` for a view that was never persisted — a `FrameworkResolver`
    /// synthesizes `RefView`s in memory, and a test may build one by hand. Such a
    /// view identifies no row, so it must never delete anything; the cleanup path
    /// skips it.
    pub row_id: Option<i64>,
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
    /// Finer structural extraction label (Godot only); `None` otherwise. Threaded
    /// like `is_function_ref` into the persisted row's `reference_subkind` column.
    pub reference_subkind: Option<ReferenceSubkind>,
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
/// Cross-file re-exports (`export { x } from './other'` / `export * from
/// './other'`) and explicit local export aliases.
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
    /// An explicit local export alias such as `export { original as alias }`.
    LocalAlias {
        /// Name as exported by THIS file.
        exported_name: String,
        /// Name of the local declaration.
        original_name: String,
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

    /// EVERY Go module in the project, for multi-`go.mod` layouts (#1521).
    ///
    /// The default lifts [`Self::get_go_module`], so a context that only knows the
    /// root module keeps its exact previous behaviour and no implementor is forced
    /// to change.
    fn get_go_modules(&self) -> Vec<GoModule> {
        self.get_go_module().into_iter().collect()
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
/// referenced by `ResolutionContext.getGoModule?` (`types.ts:104-111`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoModule {
    pub module_path: String,
    /// Project-relative directory holding this module's `go.mod`, `/`-separated,
    /// EMPTY for a module at the project root. An import path is rewritten to a
    /// directory as `dir` + the part of the import after `module_path`, so a
    /// nested module's packages resolve to their real on-disk location (#1521).
    pub dir: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Language, Node, NodeKind};

    #[test]
    fn all_node_kinds_classified_against_supertype_and_import_sets() {
        // Every one of the 23 kinds is classified explicitly, so a kind added
        // later cannot be silently omitted from either gate: the two expected
        // lists below are exhaustive and the assertion compares whole sets.
        let type_declaring: Vec<NodeKind> = NodeKind::ALL
            .into_iter()
            .filter(|k| declares_type_name(*k))
            .collect();
        assert_eq!(
            type_declaring,
            vec![
                NodeKind::Module,
                NodeKind::Class,
                NodeKind::Struct,
                NodeKind::Interface,
                NodeKind::Trait,
                NodeKind::Protocol,
                NodeKind::Enum,
                NodeKind::TypeAlias,
                NodeKind::Namespace,
                NodeKind::Component,
                NodeKind::Union,
            ],
            "11 supertype-eligible kinds, `Union` among them since Tier 3"
        );

        let non_importable: Vec<NodeKind> = NodeKind::ALL
            .into_iter()
            .filter(|k| !is_importable_kind(*k))
            .collect();
        assert_eq!(
            non_importable,
            vec![
                NodeKind::Method,
                NodeKind::Property,
                NodeKind::Field,
                NodeKind::EnumMember,
                NodeKind::Parameter,
            ]
        );

        assert_eq!(
            NodeKind::ALL.len(),
            23,
            "kind count changed — revisit both sets"
        );
    }

    #[test]
    fn kind_eligibility_gates_only_inheritance_and_imports() {
        // Inheritance refs demand a type-declaring target.
        for kind in [EdgeKind::Extends, EdgeKind::Implements] {
            assert!(is_inheritance_ref(kind));
            assert!(kind_is_eligible_target(kind, NodeKind::Trait));
            assert!(kind_is_eligible_target(kind, NodeKind::TypeAlias));
            assert!(!kind_is_eligible_target(kind, NodeKind::EnumMember));
            assert!(!kind_is_eligible_target(kind, NodeKind::Function));
        }
        // Imports refs deny type members.
        assert!(kind_is_eligible_target(
            EdgeKind::Imports,
            NodeKind::Function
        ));
        assert!(kind_is_eligible_target(EdgeKind::Imports, NodeKind::Import));
        assert!(!kind_is_eligible_target(
            EdgeKind::Imports,
            NodeKind::Property
        ));
        // Every other reference kind is unconstrained.
        assert!(!is_inheritance_ref(EdgeKind::Calls));
        for target in NodeKind::ALL {
            assert!(kind_is_eligible_target(EdgeKind::Calls, target));
            assert!(kind_is_eligible_target(EdgeKind::References, target));
        }
    }

    #[test]
    fn esm_language_set_is_the_import_required_family() {
        for language in ESM_LANGUAGES {
            assert!(is_esm_language(language), "{language:?}");
        }
        // Package/namespace-scoped languages must stay out: a sibling-file type
        // is visible there with no import, so the visibility oracle would delete
        // real edges.
        for language in [
            Language::Go,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Rust,
            Language::Scala,
            Language::Dart,
            Language::Python,
            Language::Cpp,
        ] {
            assert!(!is_esm_language(language), "{language:?}");
        }
    }

    #[test]
    fn resolved_by_as_str_all_variants() {
        assert_eq!(ResolvedBy::ExactMatch.as_str(), "exact-match");
        assert_eq!(ResolvedBy::Import.as_str(), "import");
        assert_eq!(ResolvedBy::QualifiedName.as_str(), "qualified-name");
        assert_eq!(ResolvedBy::Framework.as_str(), "framework");
        assert_eq!(ResolvedBy::Fuzzy.as_str(), "fuzzy");
        assert_eq!(ResolvedBy::InstanceMethod.as_str(), "instance-method");
        assert_eq!(ResolvedBy::FilePath.as_str(), "file-path");
        assert_eq!(ResolvedBy::FunctionRef.as_str(), "function-ref");
    }

    #[test]
    fn resolved_by_derives() {
        let a = ResolvedBy::Import;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(ResolvedBy::Fuzzy, ResolvedBy::Import);
        assert!(format!("{a:?}").contains("Import"));
    }

    fn sample_ref() -> RefView {
        RefView {
            row_id: None,
            from_node_id: "fn:1".to_string(),
            reference_name: "foo".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 5,
            file_path: "src/a.ts".to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    #[test]
    fn ref_view_constructs_and_derives() {
        let r = sample_ref();
        assert_eq!(r.clone(), r);
        assert_eq!(r.reference_kind, EdgeKind::Calls);
        assert!(format!("{r:?}").contains("RefView"));
    }

    #[test]
    fn ref_view_with_subkind() {
        let mut r = sample_ref();
        r.reference_subkind = Some(ReferenceSubkind::ScriptAttach);
        r.is_function_ref = true;
        assert_eq!(r.reference_subkind, Some(ReferenceSubkind::ScriptAttach));
        assert!(r.is_function_ref);
    }

    #[test]
    fn resolved_ref_constructs() {
        let rr = ResolvedRef {
            original: sample_ref(),
            target_node_id: "fn:2".to_string(),
            confidence: 0.75,
            resolved_by: ResolvedBy::ExactMatch,
        };
        assert_eq!(rr.clone(), rr);
        assert!((rr.confidence - 0.75).abs() < f64::EPSILON);
        assert_eq!(rr.resolved_by, ResolvedBy::ExactMatch);
    }

    #[test]
    fn resolution_stats_default_and_by_method() {
        let mut stats = ResolutionStats::default();
        assert_eq!(stats.total, 0);
        stats.total = 10;
        stats.resolved = 7;
        stats.unresolved = 3;
        stats.by_method.insert("import".to_string(), 4);
        assert_eq!(stats.by_method.get("import"), Some(&4));
        assert_eq!(stats.clone(), stats);
    }

    #[test]
    fn resolution_result_default() {
        let res = ResolutionResult::default();
        assert!(res.resolved.is_empty());
        assert!(res.unresolved.is_empty());
        assert_eq!(res.stats.total, 0);
    }

    #[test]
    fn import_mapping_constructs() {
        let im = ImportMapping {
            local_name: "X".to_string(),
            exported_name: "Y".to_string(),
            source: "./mod".to_string(),
            is_default: true,
            is_namespace: false,
        };
        assert_eq!(im.clone(), im);
        assert!(im.is_default);
        assert!(format!("{im:?}").contains("ImportMapping"));
    }

    #[test]
    fn re_export_named_and_wildcard() {
        let named = ReExport::Named {
            exported_name: "A".to_string(),
            original_name: "B".to_string(),
            source: "./x".to_string(),
        };
        let wild = ReExport::Wildcard {
            source: "./y".to_string(),
        };
        assert_eq!(named.clone(), named);
        assert_ne!(named, wild);
        assert!(format!("{wild:?}").contains("Wildcard"));
    }

    #[test]
    fn framework_extraction_result_default() {
        let r = FrameworkResolverExtractionResult::default();
        assert!(r.nodes.is_empty());
        assert!(r.references.is_empty());
        let r2 = FrameworkResolverExtractionResult {
            nodes: Vec::new(),
            references: vec![sample_ref()],
        };
        assert_eq!(r2.references.len(), 1);
    }

    #[test]
    fn go_module_constructs_and_derives() {
        let gm = GoModule {
            module_path: "example.com/mod".to_string(),
            dir: "sub".to_string(),
        };
        assert_eq!(gm.clone(), gm);
        assert_eq!(gm.module_path, "example.com/mod");
        assert_eq!(gm.dir, "sub");
        assert!(format!("{gm:?}").contains("GoModule"));
    }

    fn node(name: &str, kind: NodeKind) -> Node {
        Node {
            id: format!("{kind:?}:{name}"),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/a.ts".to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// Minimal context exercising the trait's DEFAULT method bodies (the
    /// `known_node_names` loop and the always-empty optional-method defaults),
    /// which the store/snapshot contexts override and so never cover.
    struct MiniContext {
        nodes: Vec<Node>,
    }

    impl ResolutionContext for MiniContext {
        fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
            Vec::new()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, _file_path: &str) -> bool {
            false
        }
        fn read_file(&self, _file_path: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            "/proj"
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

    #[test]
    fn default_known_node_names_loops_all_kinds() {
        let ctx = MiniContext {
            nodes: vec![
                node("Foo", NodeKind::Class),
                node("bar", NodeKind::Function),
            ],
        };
        let mut names = ctx.known_node_names();
        names.sort();
        assert_eq!(names, vec!["Foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn default_optional_methods_return_empty() {
        let ctx = MiniContext { nodes: Vec::new() };
        assert!(ctx.get_supertypes("T", Language::TypeScript).is_empty());
        assert!(ctx.get_project_aliases().is_none());
        assert!(ctx.get_workspace_packages().is_none());
        assert!(ctx.get_go_module().is_none());
        assert!(ctx.get_re_exports("f.ts", Language::TypeScript).is_empty());
        assert!(ctx.get_cpp_include_dirs().is_empty());
    }

    #[test]
    fn mini_context_required_methods_exercised() {
        let ctx = MiniContext {
            nodes: vec![node("Foo", NodeKind::Class)],
        };
        assert!(ctx.get_nodes_in_file("a.ts").is_empty());
        assert_eq!(ctx.get_nodes_by_name("Foo").len(), 1);
        assert!(ctx.get_nodes_by_qualified_name("Foo").is_empty());
        assert_eq!(ctx.get_nodes_by_kind(NodeKind::Class).len(), 1);
        assert!(!ctx.file_exists("a.ts"));
        assert!(ctx.read_file("a.ts").is_none());
        assert_eq!(ctx.get_project_root(), "/proj");
        assert!(ctx.get_all_files().is_empty());
        assert!(ctx.get_nodes_by_lower_name("foo").is_empty());
        assert!(ctx.get_node_by_id("x").is_none());
        assert!(
            ctx.get_import_mappings("a.ts", Language::TypeScript)
                .is_empty()
        );
    }
}
