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
//! same graph read surface into immutable node state plus bounded thread-safe
//! caches, so it can be shared across threads
//! (`SnapshotResolutionContext: Sync`) while producing BYTE-IDENTICAL results
//! to the store-backed context.
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
//! applying the identical [`order_candidates`](crate::context::order_candidates)
//! tie-break, so the
//! candidate order matches the store context exactly.

use crate::context::{DEFAULT_CACHE_LIMIT, order_candidates};
use crate::import_resolver;
use crate::path_aliases::{AliasMap, load_project_aliases};
use crate::types::{GoModule, ImportMapping, ReExport, ResolutionContext};
use crate::workspace_packages::{WorkspacePackages, load_workspace_packages};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

/// Edge-adjacency for [`SnapshotResolutionContext::get_supertypes`]: source
/// node id → its `implements`/`extends` `(target_id, kind)` pairs, in the
/// store's row order within each kind (Implements queried before Extends).
pub type EdgeAdjacency = Arc<HashMap<String, Vec<(String, EdgeKind)>>>;

/// One single-flight value plus the access bit used by the bounded clock cache.
struct MemoEntry<V> {
    value: OnceLock<V>,
    recently_used: AtomicBool,
}

impl<V> MemoEntry<V> {
    fn new() -> Self {
        Self {
            value: OnceLock::new(),
            recently_used: AtomicBool::new(true),
        }
    }
}

struct BoundedFileMemoState<V> {
    entries: HashMap<Arc<str>, Arc<MemoEntry<V>>>,
    order: VecDeque<Arc<str>>,
}

/// Bounded, read-concurrent file-keyed memoisation.
///
/// Hits take only a shared lock and an atomic access-bit update. Misses install
/// a [`OnceLock`] before doing I/O or parsing, so concurrent callers for the
/// same file share one initialization. The clock eviction policy approximates
/// the upstream LRU without the existing `VecDeque::position` scan on every hit.
struct BoundedFileMemo<V> {
    max_entries: usize,
    state: RwLock<BoundedFileMemoState<V>>,
}

impl<V> BoundedFileMemo<V>
where
    V: Clone,
{
    fn new(max_entries: usize) -> Self {
        assert!(
            max_entries > 0,
            "BoundedFileMemo max_entries must be positive"
        );
        Self {
            max_entries,
            state: RwLock::new(BoundedFileMemoState {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    fn get_or_init(&self, file_path: &str, init: impl FnOnce() -> V) -> V {
        let cached = {
            let state = self
                .state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.entries.get(file_path).cloned()
        };
        let entry = match cached {
            Some(entry) => {
                entry.recently_used.store(true, Ordering::Relaxed);
                entry
            }
            None => {
                let mut state = self
                    .state
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(entry) = state.entries.get(file_path).cloned() {
                    entry.recently_used.store(true, Ordering::Relaxed);
                    entry
                } else {
                    while state.entries.len() >= self.max_entries {
                        let Some(oldest) = state.order.pop_front() else {
                            state.entries.clear();
                            break;
                        };
                        let Some(oldest_entry) = state.entries.get(oldest.as_ref()).cloned() else {
                            continue;
                        };
                        if oldest_entry.recently_used.swap(false, Ordering::Relaxed) {
                            state.order.push_back(oldest);
                        } else {
                            state.entries.remove(oldest.as_ref());
                        }
                    }

                    let key: Arc<str> = Arc::from(file_path);
                    let entry = Arc::new(MemoEntry::new());
                    state.entries.insert(Arc::clone(&key), Arc::clone(&entry));
                    state.order.push_back(key);
                    entry
                }
            }
        };

        entry.value.get_or_init(init).clone()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }
}

#[cfg(test)]
#[derive(Default)]
struct SnapshotCacheCounters {
    file_loads: AtomicUsize,
    import_parses: AtomicUsize,
    re_export_parses: AtomicUsize,
}

/// Whole-run file-derived caches shared by every per-chunk context clone.
struct SnapshotCaches {
    file_contents: BoundedFileMemo<Option<Arc<str>>>,
    import_mappings: BoundedFileMemo<Arc<Vec<ImportMapping>>>,
    re_exports: BoundedFileMemo<Arc<Vec<ReExport>>>,
    #[cfg(test)]
    counters: SnapshotCacheCounters,
}

impl SnapshotCaches {
    fn new() -> Self {
        // Match the upstream/store budgets: full source text gets one fifth of
        // the normal cache because it dominates retained memory.
        let content_limit = std::cmp::max(64, DEFAULT_CACHE_LIMIT / 5);
        Self {
            file_contents: BoundedFileMemo::new(content_limit),
            import_mappings: BoundedFileMemo::new(DEFAULT_CACHE_LIMIT),
            re_exports: BoundedFileMemo::new(DEFAULT_CACHE_LIMIT),
            #[cfg(test)]
            counters: SnapshotCacheCounters::default(),
        }
    }
}

/// WHOLE-RUN immutable node + project-config snapshot, shared across chunks.
///
/// Built ONCE per resolve pass (after framework extraction injects its nodes —
/// see T4) and never mutated, so it is cheap to clone (`Arc` bumps only) and
/// safe to share across `rayon` threads.
struct NodeSnapshot {
    project_root: String,
    by_name: HashMap<String, Vec<Arc<Node>>>,
    by_lower_name: HashMap<String, Vec<Arc<Node>>>,
    by_qualified_name: HashMap<String, Vec<Arc<Node>>>,
    by_kind: HashMap<NodeKind, Vec<Arc<Node>>>,
    by_file_path: HashMap<String, Vec<Arc<Node>>>,
    by_id: HashMap<String, Arc<Node>>,
    known_node_names: Vec<String>,
    known_file_paths: HashSet<String>,
    all_file_paths: Arc<Vec<String>>,
    files_by_basename: HashMap<String, Arc<Vec<String>>>,
    project_aliases: Option<AliasMap>,
    workspace_packages: Option<WorkspacePackages>,
    go_module: Option<GoModule>,
    go_modules: Vec<GoModule>,
}

/// A `Sync`, immutable [`ResolutionContext`] over a precomputed [`NodeSnapshot`]
/// plus a per-chunk [`EdgeAdjacency`] map.
///
/// Mirrors the read surface of
/// [`StoreResolutionContext`](crate::context::StoreResolutionContext) without a
/// live store handle, so it is `Sync` and usable from a `rayon` parallel map.
/// File-derived methods share bounded, single-flight run caches; candidate
/// ordering, parsing, and aliasing still match the store context.
pub struct SnapshotResolutionContext {
    snapshot: Arc<NodeSnapshot>,
    caches: Arc<SnapshotCaches>,
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

        let mut by_name: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        let mut by_lower_name: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        let mut by_qualified_name: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        let mut by_id: HashMap<String, Arc<Node>> = HashMap::with_capacity(nodes.len());

        for node in nodes {
            let node = Arc::new(node);
            by_name
                .entry(node.name.clone())
                .or_default()
                .push(Arc::clone(&node));
            by_lower_name
                .entry(node.name.to_lowercase())
                .or_default()
                .push(Arc::clone(&node));
            by_qualified_name
                .entry(node.qualified_name.clone())
                .or_default()
                .push(Arc::clone(&node));
            by_id.insert(node.id.clone(), node);
        }
        for list in by_name.values_mut() {
            order_candidates(list);
        }
        for list in by_lower_name.values_mut() {
            order_candidates(list);
        }
        for list in by_qualified_name.values_mut() {
            order_candidates(list);
        }

        // Per-file and per-kind lists copy the store's own query output verbatim
        // so the candidate order matches `nodes_by_file_path` (ORDER BY
        // start_line) and `nodes_by_kind` (SQLite scan order) exactly.
        let mut file_paths: Vec<String> = by_id.values().map(|n| n.file_path.clone()).collect();
        file_paths.sort_unstable();
        file_paths.dedup();
        let mut by_file_path: HashMap<String, Vec<Arc<Node>>> =
            HashMap::with_capacity(file_paths.len());
        for fp in &file_paths {
            let entries = store
                .nodes_by_file_path(fp)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|node| by_id.get(&node.id).cloned())
                .collect();
            by_file_path.insert(fp.clone(), entries);
        }

        let mut by_kind: HashMap<NodeKind, Vec<Arc<Node>>> = HashMap::new();
        for kind in NodeKind::ALL {
            let entries = store
                .nodes_by_kind(kind)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|node| by_id.get(&node.id).cloned())
                .collect();
            by_kind.insert(kind, entries);
        }

        let known_node_names = store.all_node_names().unwrap_or_default();

        let known_file_paths: HashSet<String> = store
            .all_files()
            .map(|files| files.into_iter().map(|f| f.path).collect())
            .unwrap_or_default();
        let mut all_file_paths: Vec<String> = known_file_paths.iter().cloned().collect();
        // `all_files` returns `ORDER BY path`; mirror it for `get_all_files`.
        all_file_paths.sort();
        let mut files_by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for file in &all_file_paths {
            let basename = file
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(file.as_str())
                .to_string();
            files_by_basename
                .entry(basename)
                .or_default()
                .push(file.clone());
        }
        let files_by_basename = files_by_basename
            .into_iter()
            .map(|(name, files)| (name, Arc::new(files)))
            .collect();
        let all_file_paths = Arc::new(all_file_paths);

        let project_aliases = load_project_aliases(&project_root);
        let workspace_packages = load_workspace_packages(&project_root);
        let go_module = crate::context::load_go_module_pub(&project_root);
        let go_modules = crate::context::load_go_modules_pub(&project_root, &all_file_paths);

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
                files_by_basename,
                project_aliases,
                workspace_packages,
                go_module,
                go_modules,
            }),
            caches: Arc::new(SnapshotCaches::new()),
            edges: Arc::new(HashMap::new()),
        })
    }

    fn read_file_cached(&self, file_path: &str) -> Option<Arc<str>> {
        self.caches.file_contents.get_or_init(file_path, || {
            #[cfg(test)]
            self.caches
                .counters
                .file_loads
                .fetch_add(1, Ordering::Relaxed);
            let full_path = Path::new(&self.snapshot.project_root).join(file_path);
            std::fs::read_to_string(full_path).ok().map(Arc::from)
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
            caches: Arc::clone(&self.caches),
            edges,
        }
    }

    #[cfg(test)]
    fn cache_counts(&self) -> (usize, usize, usize) {
        (
            self.caches.counters.file_loads.load(Ordering::Relaxed),
            self.caches.counters.import_parses.load(Ordering::Relaxed),
            self.caches
                .counters
                .re_export_parses
                .load(Ordering::Relaxed),
        )
    }
}

impl ResolutionContext for SnapshotResolutionContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.get_nodes_in_file_shared(file_path)
            .into_iter()
            .map(|node| node.as_ref().clone())
            .collect()
    }

    fn get_nodes_in_file_shared(&self, file_path: &str) -> Vec<Arc<Node>> {
        self.snapshot
            .by_file_path
            .get(file_path)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.get_nodes_by_name_shared(name)
            .into_iter()
            .map(|node| node.as_ref().clone())
            .collect()
    }

    fn get_nodes_by_name_shared(&self, name: &str) -> Vec<Arc<Node>> {
        self.snapshot.by_name.get(name).cloned().unwrap_or_default()
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.get_nodes_by_qualified_name_shared(qualified_name)
            .into_iter()
            .map(|node| node.as_ref().clone())
            .collect()
    }

    fn get_nodes_by_qualified_name_shared(&self, qualified_name: &str) -> Vec<Arc<Node>> {
        self.snapshot
            .by_qualified_name
            .get(qualified_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.get_nodes_by_kind_shared(kind)
            .into_iter()
            .map(|node| node.as_ref().clone())
            .collect()
    }

    fn get_nodes_by_kind_shared(&self, kind: NodeKind) -> Vec<Arc<Node>> {
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
        self.read_file_cached(file_path)
            .as_deref()
            .map(str::to_owned)
    }

    fn is_file_readable(&self, file_path: &str) -> bool {
        self.read_file_cached(file_path).is_some()
    }

    fn get_project_root(&self) -> &str {
        &self.snapshot.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.snapshot.all_file_paths.as_ref().clone()
    }

    fn get_all_files_shared(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.snapshot.all_file_paths)
    }

    fn get_files_by_basename_shared(&self, basename: &str) -> Arc<Vec<String>> {
        self.snapshot
            .files_by_basename
            .get(basename)
            .cloned()
            .unwrap_or_else(|| Arc::new(Vec::new()))
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.get_nodes_by_lower_name_shared(lower_name)
            .into_iter()
            .map(|node| node.as_ref().clone())
            .collect()
    }

    fn get_nodes_by_lower_name_shared(&self, lower_name: &str) -> Vec<Arc<Node>> {
        self.snapshot
            .by_lower_name
            .get(lower_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.get_node_by_id_shared(id)
            .map(|node| node.as_ref().clone())
    }

    fn get_node_by_id_shared(&self, id: &str) -> Option<Arc<Node>> {
        self.snapshot.by_id.get(id).cloned()
    }

    fn get_supertypes(&self, type_name: &str, language: Language) -> Vec<String> {
        // Union implements/extends targets of every same-named type node,
        // reading the per-chunk edge adjacency instead of the live store
        // (matches `StoreResolutionContext::get_supertypes`).
        const SUPERTYPE_BEARING: [NodeKind; 7] = [
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Union,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Protocol,
            NodeKind::Enum,
        ];
        let type_nodes = self
            .get_nodes_by_name_shared(type_name)
            .into_iter()
            .filter(|n| SUPERTYPE_BEARING.contains(&n.kind) && n.language == language)
            .collect::<Vec<_>>();
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
        self.caches
            .import_mappings
            .get_or_init(file_path, || {
                #[cfg(test)]
                self.caches
                    .counters
                    .import_parses
                    .fetch_add(1, Ordering::Relaxed);
                Arc::new(
                    self.read_file_cached(file_path)
                        .map(|text| import_resolver::extract_import_mappings(&text, language))
                        .unwrap_or_default(),
                )
            })
            .as_ref()
            .clone()
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

    fn get_go_modules(&self) -> Vec<GoModule> {
        self.snapshot.go_modules.clone()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        self.caches
            .re_exports
            .get_or_init(file_path, || {
                #[cfg(test)]
                self.caches
                    .counters
                    .re_export_parses
                    .fetch_add(1, Ordering::Relaxed);
                Arc::new(match self.read_file_cached(file_path) {
                    Some(text) => {
                        // Re-key on the BARREL file's own extension (matches the
                        // store context: js-family files parse as TypeScript).
                        let lang = if crate::context::is_js_family_path_pub(file_path) {
                            Language::TypeScript
                        } else {
                            language
                        };
                        import_resolver::extract_re_exports(&text, lang)
                    }
                    None => Vec::new(),
                })
            })
            .as_ref()
            .clone()
    }
}

/// Build the per-chunk [`EdgeAdjacency`] map from the live store: every
/// `implements`/`extends` edge of every node, grouped by source id in
/// `edges_by_source_kind` order (Implements before Extends). T4 calls this
/// before each chunk's parallel resolve so [`SnapshotResolutionContext::get_supertypes`]
/// sees the same edges the store context would.
pub fn build_edge_adjacency(store: &Store) -> anyhow::Result<EdgeAdjacency> {
    let mut adjacency: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
    // Two graph-wide indexed queries replace two queries PER NODE. Query kinds
    // in this order to preserve the old adjacency ordering exactly.
    for edge_kind in [EdgeKind::Implements, EdgeKind::Extends] {
        for edge in store.edges_by_kind(edge_kind)? {
            adjacency
                .entry(edge.source)
                .or_default()
                .push((edge.target, edge.kind));
        }
    }
    Ok(Arc::new(adjacency))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Edge, FileRecord};

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("cg-snap-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).expect("mkdir");
        p
    }

    fn node(id: &str, name: &str, kind: NodeKind, file: &str, line: i64, col: i64) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: line,
            end_line: line,
            start_column: col,
            end_column: col,
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

    fn file_record(path: &str, count: i64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: "h".to_string(),
            language: Language::TypeScript,
            size: 0,
            modified_at: 0,
            indexed_at: 0,
            node_count: count,
            errors: Vec::new(),
            generated: false,
        }
    }

    fn populated_store(root: &std::path::Path) -> Store {
        let mut store = Store::open(&root.join("index.db")).expect("open");
        store.upsert_file(&file_record("a.ts", 2)).unwrap();
        store
            .upsert_nodes(&[
                node("child", "Child", NodeKind::Class, "a.ts", 1, 0),
                node("base", "Base", NodeKind::Interface, "a.ts", 5, 0),
            ])
            .unwrap();
        store
            .insert_edges(&[Edge {
                id: None,
                source: "child".to_string(),
                target: "base".to_string(),
                kind: EdgeKind::Implements,
                metadata: None,
                line: Some(1),
                col: Some(0),
                provenance: None,
            }])
            .unwrap();
        store
    }

    #[test]
    fn from_store_builds_node_lookups() {
        let root = temp_root("build");
        let store = populated_store(&root);
        let ctx = SnapshotResolutionContext::from_store(&store, root.to_str().unwrap())
            .expect("snapshot");

        assert_eq!(ctx.get_nodes_in_file("a.ts").len(), 2);
        assert_eq!(ctx.get_nodes_by_name("Child").len(), 1);
        assert_eq!(ctx.get_nodes_by_qualified_name("Child").len(), 1);
        assert_eq!(ctx.get_nodes_by_kind(NodeKind::Class).len(), 1);
        assert_eq!(ctx.get_nodes_by_lower_name("child").len(), 1);
        assert!(ctx.get_node_by_id("child").is_some());
        assert!(ctx.get_node_by_id("missing").is_none());
        let by_name = ctx.get_nodes_by_name_shared("Child");
        let by_file = ctx.get_nodes_in_file_shared("a.ts");
        let by_kind = ctx.get_nodes_by_kind_shared(NodeKind::Class);
        let by_id = ctx.get_node_by_id_shared("child").expect("shared id");
        assert!(Arc::ptr_eq(&by_name[0], &by_file[0]));
        assert!(Arc::ptr_eq(&by_name[0], &by_kind[0]));
        assert!(Arc::ptr_eq(&by_name[0], &by_id));
        assert!(ctx.known_node_names().contains(&"Child".to_string()));
        assert_eq!(ctx.get_project_root(), root.to_str().unwrap());
        assert!(ctx.get_nodes_in_file("nope.ts").is_empty());
        assert!(ctx.get_nodes_by_name("Nope").is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_exists_uses_known_set_and_fs() {
        let root = temp_root("exists");
        let store = populated_store(&root);
        std::fs::write(root.join("ondisk.ts"), "x").unwrap();
        let ctx = SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap();
        assert!(ctx.file_exists("a.ts"));
        assert!(ctx.file_exists("ondisk.ts"));
        assert!(!ctx.file_exists("nowhere.ts"));
        assert!(ctx.get_all_files().contains(&"a.ts".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_file_and_import_and_re_exports_are_cached() {
        let root = temp_root("read");
        let store = Store::open(&root.join("index.db")).unwrap();
        let original =
            "import { foo } from './c';\nexport { foo } from './c';\nexport * from './d';\n";
        std::fs::write(root.join("b.ts"), original).unwrap();
        let ctx = SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap();

        assert_eq!(ctx.read_file("b.ts").as_deref(), Some(original));
        let imports = ctx.get_import_mappings("b.ts", Language::TypeScript);
        let re_exports = ctx.get_re_exports("b.ts", Language::TypeScript);
        assert!(!imports.is_empty());
        assert!(!re_exports.is_empty());

        // All three caches retain the first immutable-run view, including across
        // the per-chunk context clones used by the parallel resolver.
        std::fs::write(
            root.join("b.ts"),
            "import { changed } from './new';\nexport { changed } from './new';\n",
        )
        .unwrap();
        let cloned = ctx.with_edge_adjacency(Arc::new(HashMap::new()));
        assert_eq!(cloned.read_file("b.ts").as_deref(), Some(original));
        assert_eq!(
            cloned.get_import_mappings("b.ts", Language::TypeScript),
            imports
        );
        assert_eq!(
            cloned.get_re_exports("b.ts", Language::TypeScript),
            re_exports
        );

        assert!(ctx.read_file("missing.ts").is_none());
        assert!(!ctx.is_file_readable("missing.ts"));
        assert!(
            ctx.get_import_mappings("gone.ts", Language::TypeScript)
                .is_empty()
        );
        assert!(
            ctx.get_re_exports("gone.ts", Language::TypeScript)
                .is_empty()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bounded_file_memo_is_single_flight_and_bounded() {
        let cache = Arc::new(BoundedFileMemo::new(2));
        let starts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let starts = Arc::clone(&starts);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                cache.get_or_init("shared.ts", || {
                    starts.fetch_add(1, Ordering::Relaxed);
                    7
                })
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap(), 7);
        }
        assert_eq!(starts.load(Ordering::Relaxed), 1);

        assert_eq!(cache.get_or_init("second.ts", || 2), 2);
        assert_eq!(cache.get_or_init("third.ts", || 3), 3);
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache.get_or_init("shared.ts", || {
                starts.fetch_add(1, Ordering::Relaxed);
                9
            }),
            9,
            "the oldest entry is reloaded after bounded eviction"
        );
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn parallel_file_derived_lookups_initialize_once() {
        let root = temp_root("parallel-cache");
        let store = Store::open(&root.join("index.db")).unwrap();
        std::fs::write(
            root.join("b.ts"),
            "import { foo } from './c';\nexport { foo } from './c';\n",
        )
        .unwrap();
        let ctx = Arc::new(
            SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap(),
        );
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let mut workers = Vec::new();
        for _ in 0..16 {
            let ctx = Arc::clone(&ctx);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                assert!(ctx.read_file("b.ts").is_some());
                assert!(
                    !ctx.get_import_mappings("b.ts", Language::TypeScript)
                        .is_empty()
                );
                assert!(!ctx.get_re_exports("b.ts", Language::TypeScript).is_empty());
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(
            ctx.cache_counts(),
            (1, 1, 1),
            "file I/O and both parsers are single-flight across rayon-style callers"
        );
        let cloned = ctx.with_edge_adjacency(Arc::new(HashMap::new()));
        assert!(cloned.read_file("b.ts").is_some());
        assert!(
            !cloned
                .get_import_mappings("b.ts", Language::TypeScript)
                .is_empty()
        );
        assert!(
            !cloned
                .get_re_exports("b.ts", Language::TypeScript)
                .is_empty()
        );
        assert_eq!(cloned.cache_counts(), (1, 1, 1));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn get_supertypes_empty_without_edges() {
        let root = temp_root("noedges");
        let store = populated_store(&root);
        let ctx = SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap();
        // No edge adjacency installed yet → empty, matching the store context
        // before edges are persisted.
        assert!(ctx.get_supertypes("Child", Language::TypeScript).is_empty());
        assert!(ctx.get_supertypes("Nope", Language::TypeScript).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn get_supertypes_reads_installed_adjacency() {
        let root = temp_root("edges");
        let store = populated_store(&root);
        let adjacency = build_edge_adjacency(&store).expect("adjacency");
        assert!(adjacency.contains_key("child"));

        let base = SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap();
        // `with_edge_adjacency` shares the node snapshot but swaps the edge map.
        let ctx = base.with_edge_adjacency(Arc::clone(&adjacency));
        assert_eq!(
            ctx.get_supertypes("Child", Language::TypeScript),
            vec!["Base".to_string()]
        );

        // `set_edge_adjacency` mutates in place with the same effect.
        let mut owned =
            SnapshotResolutionContext::from_store(&store, root.to_str().unwrap()).unwrap();
        owned.set_edge_adjacency(adjacency);
        assert_eq!(
            owned.get_supertypes("Child", Language::TypeScript),
            vec!["Base".to_string()]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_edge_adjacency_empty_store() {
        let root = temp_root("emptyadj");
        let store = Store::open(&root.join("index.db")).unwrap();
        let adjacency = build_edge_adjacency(&store).expect("adjacency");
        assert!(adjacency.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_edge_adjacency_bulk_query_keeps_kind_order() {
        let root = temp_root("adj-order");
        let mut store = Store::open(&root.join("index.db")).unwrap();
        store.upsert_file(&file_record("a.ts", 3)).unwrap();
        store
            .upsert_nodes(&[
                node("child", "Child", NodeKind::Class, "a.ts", 1, 0),
                node(
                    "implemented",
                    "Implemented",
                    NodeKind::Interface,
                    "a.ts",
                    2,
                    0,
                ),
                node("extended", "Extended", NodeKind::Class, "a.ts", 3, 0),
            ])
            .unwrap();
        store
            .insert_edges(&[
                Edge {
                    id: None,
                    source: "child".to_string(),
                    target: "extended".to_string(),
                    kind: EdgeKind::Extends,
                    metadata: None,
                    line: Some(1),
                    col: Some(0),
                    provenance: None,
                },
                Edge {
                    id: None,
                    source: "child".to_string(),
                    target: "implemented".to_string(),
                    kind: EdgeKind::Implements,
                    metadata: None,
                    line: Some(1),
                    col: Some(0),
                    provenance: None,
                },
            ])
            .unwrap();

        let adjacency = build_edge_adjacency(&store).unwrap();
        assert_eq!(
            adjacency.get("child").unwrap(),
            &vec![
                ("implemented".to_string(), EdgeKind::Implements),
                ("extended".to_string(), EdgeKind::Extends),
            ]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_context_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<SnapshotResolutionContext>();
    }
}
