//! Immutable, `Sync` [`ResolutionContext`] for parallel resolution.
//!
// allow: SIZE_OK — the ~17-method `ResolutionContext` trait impl is an
// indivisible unit (one impl block must cover the whole trait); splitting the
// edge-adjacency builder from the type it populates would only fragment a
// cohesive single-responsibility module.
//!
//! [`StoreResolutionContext`](crate::context::StoreResolutionContext) holds a
//! live `&Store` handle behind `RefCell<Caches>` (LRU memoisation), so it is
//! NOT `Sync` and cannot back a `rayon` parallel resolve. This type captures the
//! same read surface into immutable, precomputed `Arc` state so it can be shared
//! across threads (`SnapshotResolutionContext: Sync`) while producing
//! BYTE-IDENTICAL results to the store-backed context.
//!
//! The snapshot is split in two:
//!   * a WHOLE-RUN immutable part (nodes + project config), built once from the
//!     live store — nodes are static during a resolve pass; and
//!   * a PER-CHUNK edge-adjacency part for [`get_supertypes`], swapped in by the
//!     batched resolver before each chunk's parallel resolve (T4) because the
//!     batched resolver inserts `implements`/`extends` edges per chunk and the
//!     main pass must see the growth.
//!
//! Each node-lookup `Vec` is built by copying the store's own query output
//! verbatim (`nodes_by_file_path`, `nodes_by_kind`, `all_node_names`) or by
//! applying the identical [`order_candidates`](crate::context) tie-break, so the
//! candidate order matches the store context exactly.

use crate::context::order_candidates_pub;
use crate::import_resolver;
use crate::path_aliases::{load_project_aliases, AliasMap};
use crate::types::{GoModule, ImportMapping, ReExport, ResolutionContext};
use crate::workspace_packages::{load_workspace_packages, WorkspacePackages};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// Edge-adjacency for [`SnapshotResolutionContext::get_supertypes`]: source
/// node id → its `implements`/`extends` `(target_id, kind)` pairs, in the
/// store's `edges_by_source_kind` order (Implements queried before Extends).
pub type EdgeAdjacency = Arc<HashMap<String, Vec<(String, EdgeKind)>>>;

/// WHOLE-RUN immutable node + project-config snapshot, shared across chunks.
///
/// Built ONCE per resolve pass (after framework extraction injects its nodes —
/// see T4) and never mutated, so it is cheap to clone (`Arc` bumps only) and
/// safe to share across `rayon` threads.
struct NodeSnapshot {
    project_root: String,
    by_name: HashMap<String, Vec<Node>>,
    by_lower_name: HashMap<String, Vec<Node>>,
    by_qualified_name: HashMap<String, Vec<Node>>,
    by_kind: HashMap<NodeKind, Vec<Node>>,
    by_file_path: HashMap<String, Vec<Node>>,
    by_id: HashMap<String, Node>,
    known_node_names: Vec<String>,
    known_file_paths: HashSet<String>,
    all_file_paths: Vec<String>,
    project_aliases: Option<AliasMap>,
    workspace_packages: Option<WorkspacePackages>,
    go_module: Option<GoModule>,
}

/// A `Sync`, immutable [`ResolutionContext`] over a precomputed [`NodeSnapshot`]
/// plus a per-chunk [`EdgeAdjacency`] map.
///
/// Mirrors the read surface of
/// [`StoreResolutionContext`](crate::context::StoreResolutionContext) without
/// any interior mutability or live store handle, so it is `Sync` and usable from
/// a `rayon` parallel map. File-derived methods (`read_file`,
/// `get_import_mappings`, `get_re_exports`) read the immutable filesystem
/// directly (thread-safe `std::fs`), exactly as the store context does on a
/// cache miss; the candidate ordering, parsing, and aliasing all match.
pub struct SnapshotResolutionContext {
    snapshot: Arc<NodeSnapshot>,
    /// Per-chunk `implements`/`extends` adjacency for [`Self::get_supertypes`].
    /// Empty until the resolver installs a chunk's map (T4); an empty map yields
    /// the same result the store context gives before any such edge exists.
    edges: EdgeAdjacency,
}

impl SnapshotResolutionContext {
    /// Build the WHOLE-RUN snapshot from the live `store` rooted at
    /// `project_root`, with an empty per-chunk edge map.
    ///
    /// Reads every node once and loads the project aliases / go module /
    /// workspace packages once. The per-file and per-kind candidate lists are
    /// copied from the store's own queries (`nodes_by_file_path`,
    /// `nodes_by_kind`) so their order is byte-identical; the name-keyed lists
    /// apply the same [`order_candidates`](crate::context) tie-break.
    pub fn from_store(store: &Store, project_root: impl Into<String>) -> anyhow::Result<Self> {
        let project_root = project_root.into();
        let nodes = store.all_nodes()?;

        let mut by_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut by_lower_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut by_qualified_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut by_id: HashMap<String, Node> = HashMap::with_capacity(nodes.len());

        for node in &nodes {
            by_name
                .entry(node.name.clone())
                .or_default()
                .push(node.clone());
            by_lower_name
                .entry(node.name.to_lowercase())
                .or_default()
                .push(node.clone());
            by_qualified_name
                .entry(node.qualified_name.clone())
                .or_default()
                .push(node.clone());
            by_id.insert(node.id.clone(), node.clone());
        }
        for list in by_name.values_mut() {
            order_candidates_pub(list);
        }
        for list in by_lower_name.values_mut() {
            order_candidates_pub(list);
        }
        for list in by_qualified_name.values_mut() {
            order_candidates_pub(list);
        }

        // Per-file and per-kind lists copy the store's own query output verbatim
        // so the candidate order matches `nodes_by_file_path` (ORDER BY
        // start_line) and `nodes_by_kind` (SQLite scan order) exactly.
        let mut file_paths: Vec<String> = nodes.iter().map(|n| n.file_path.clone()).collect();
        file_paths.sort_unstable();
        file_paths.dedup();
        let mut by_file_path: HashMap<String, Vec<Node>> = HashMap::with_capacity(file_paths.len());
        for fp in &file_paths {
            by_file_path.insert(fp.clone(), store.nodes_by_file_path(fp).unwrap_or_default());
        }

        let mut by_kind: HashMap<NodeKind, Vec<Node>> = HashMap::new();
        for kind in NodeKind::ALL {
            by_kind.insert(kind, store.nodes_by_kind(kind).unwrap_or_default());
        }

        let known_node_names = store.all_node_names().unwrap_or_default();

        let known_file_paths: HashSet<String> = store
            .all_files()
            .map(|files| files.into_iter().map(|f| f.path).collect())
            .unwrap_or_default();
        let mut all_file_paths: Vec<String> = known_file_paths.iter().cloned().collect();
        // `all_files` returns `ORDER BY path`; mirror it for `get_all_files`.
        all_file_paths.sort();

        let project_aliases = load_project_aliases(&project_root);
        let workspace_packages = load_workspace_packages(&project_root);
        let go_module = crate::context::load_go_module_pub(&project_root);

        Ok(Self {
            snapshot: Arc::new(NodeSnapshot {
                project_root,
                by_name,
                by_lower_name,
                by_qualified_name,
                by_kind,
                by_file_path,
                by_id,
                known_node_names,
                known_file_paths,
                all_file_paths,
                project_aliases,
                workspace_packages,
                go_module,
            }),
            edges: Arc::new(HashMap::new()),
        })
    }

    /// Install the per-chunk `implements`/`extends` edge-adjacency map that
    /// [`Self::get_supertypes`] reads. Cheap (`Arc` swap); the resolver rebuilds
    /// it from the live store before each chunk's parallel resolve (T4).
    pub fn set_edge_adjacency(&mut self, edges: EdgeAdjacency) {
        self.edges = edges;
    }

    /// A clone sharing the same WHOLE-RUN node snapshot but carrying `edges` as
    /// the per-chunk adjacency. Lets the resolver derive a per-chunk context
    /// without rebuilding the node maps.
    pub fn with_edge_adjacency(&self, edges: EdgeAdjacency) -> Self {
        Self {
            snapshot: Arc::clone(&self.snapshot),
            edges,
        }
    }
}

impl ResolutionContext for SnapshotResolutionContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.snapshot
            .by_file_path
            .get(file_path)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.snapshot.by_name.get(name).cloned().unwrap_or_default()
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.snapshot
            .by_qualified_name
            .get(qualified_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.snapshot
            .by_kind
            .get(&kind)
            .cloned()
            .unwrap_or_default()
    }

    fn known_node_names(&self) -> Vec<String> {
        self.snapshot.known_node_names.clone()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        // Known-file fast path then filesystem fallback (matches the store
        // context: store-known files, normalized variant, then FS probe).
        if self.snapshot.known_file_paths.contains(file_path) {
            return true;
        }
        let normalized = file_path.replace('\\', "/");
        if normalized != file_path && self.snapshot.known_file_paths.contains(&normalized) {
            return true;
        }
        Path::new(&self.snapshot.project_root)
            .join(file_path)
            .exists()
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        let full_path = Path::new(&self.snapshot.project_root).join(file_path);
        std::fs::read_to_string(&full_path).ok()
    }

    fn get_project_root(&self) -> &str {
        &self.snapshot.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.snapshot.all_file_paths.clone()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.snapshot
            .by_lower_name
            .get(lower_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.snapshot.by_id.get(id).cloned()
    }

    fn get_supertypes(&self, type_name: &str, language: Language) -> Vec<String> {
        // Union implements/extends targets of every same-named type node,
        // reading the per-chunk edge adjacency instead of the live store
        // (matches `StoreResolutionContext::get_supertypes`).
        const SUPERTYPE_BEARING: [NodeKind; 6] = [
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Protocol,
            NodeKind::Enum,
        ];
        let type_nodes: Vec<Node> = self
            .get_nodes_by_name(type_name)
            .into_iter()
            .filter(|n| SUPERTYPE_BEARING.contains(&n.kind) && n.language == language)
            .collect();
        if type_nodes.is_empty() {
            return Vec::new();
        }
        let mut supertypes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for tn in &type_nodes {
            let Some(adjacency) = self.edges.get(&tn.id) else {
                continue;
            };
            for (target_id, edge_kind) in adjacency {
                if !matches!(edge_kind, EdgeKind::Implements | EdgeKind::Extends) {
                    continue;
                }
                if let Some(target) = self.snapshot.by_id.get(target_id) {
                    if !target.name.is_empty() && target.name != type_name {
                        supertypes.insert(target.name.clone());
                    }
                }
            }
        }
        supertypes.into_iter().collect()
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        match self.read_file(file_path) {
            Some(text) => import_resolver::extract_import_mappings(&text, language),
            None => Vec::new(),
        }
    }

    fn get_project_aliases(&self) -> Option<AliasMap> {
        self.snapshot.project_aliases.clone()
    }

    fn get_workspace_packages(&self) -> Option<WorkspacePackages> {
        self.snapshot.workspace_packages.clone()
    }

    fn get_go_module(&self) -> Option<GoModule> {
        self.snapshot.go_module.clone()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        match self.read_file(file_path) {
            Some(text) => {
                // Re-key on the BARREL file's own extension (matches the store
                // context: js-family files parse as TypeScript).
                let lang = if crate::context::is_js_family_path_pub(file_path) {
                    Language::TypeScript
                } else {
                    language
                };
                import_resolver::extract_re_exports(&text, lang)
            }
            None => Vec::new(),
        }
    }
}

/// Build the per-chunk [`EdgeAdjacency`] map from the live store: every
/// `implements`/`extends` edge of every node, grouped by source id in
/// `edges_by_source_kind` order (Implements before Extends). T4 calls this
/// before each chunk's parallel resolve so [`SnapshotResolutionContext::get_supertypes`]
/// sees the same edges the store context would.
pub fn build_edge_adjacency(store: &Store) -> anyhow::Result<EdgeAdjacency> {
    let mut adjacency: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
    for node in store.all_nodes()? {
        for edge_kind in [EdgeKind::Implements, EdgeKind::Extends] {
            let edges = store
                .edges_by_source_kind(&node.id, Some(edge_kind))
                .unwrap_or_default();
            for edge in edges {
                adjacency
                    .entry(node.id.clone())
                    .or_default()
                    .push((edge.target, edge.kind));
            }
        }
    }
    Ok(Arc::new(adjacency))
}
