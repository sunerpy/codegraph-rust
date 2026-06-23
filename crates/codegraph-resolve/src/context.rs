//! Store-backed [`ResolutionContext`].
//!
//! Ports the SQLite-backed context created in
//! `upstream resolution/index.ts:329-505` (the `createContext`
//! closure). Node lookups go through the indexed [`Store`] queries (ported in
//! Task 10, `db/queries.ts`); per-call results are memoised through the same
//! per-resolver LRU caches the upstream uses (`index.ts:217-250`). Resolution is sync.

use crate::lru_cache::LruCache;
use crate::path_aliases::{load_project_aliases, AliasMap};
use crate::types::{GoModule, ImportMapping, ReExport, ResolutionContext};
use crate::workspace_packages::{load_workspace_packages, WorkspacePackages};
use crate::{import_resolver, pathutil};
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;
use std::cell::RefCell;
use std::path::Path;

/// `DEFAULT_CACHE_LIMIT` (`index.ts:54`). Override via env in the upstream; the v1 port
/// keeps the fixed default since resolution is a single batch.
const DEFAULT_CACHE_LIMIT: usize = 5_000;

/// A [`ResolutionContext`] over a [`Store`] plus the project root.
///
/// Borrows the store immutably (reads only) — the resolver persists edges through
/// a separate `&mut Store` handle after a pass, mirroring the upstream read-context /
/// write-`queries` split.
pub struct StoreResolutionContext<'a> {
    store: &'a Store,
    project_root: String,
    caches: RefCell<Caches>,
}

struct Caches {
    node_cache: LruCache<String, Vec<Node>>,
    file_cache: LruCache<String, Option<String>>,
    import_mapping_cache: LruCache<String, Vec<ImportMapping>>,
    re_export_cache: LruCache<String, Vec<ReExport>>,
    name_cache: LruCache<String, Vec<Node>>,
    lower_name_cache: LruCache<String, Vec<Node>>,
    qualified_name_cache: LruCache<String, Vec<Node>>,
    project_aliases: Option<Option<AliasMap>>,
    go_module: Option<Option<GoModule>>,
    workspace_packages: Option<Option<WorkspacePackages>>,
}

impl Caches {
    fn new() -> Self {
        let limit = DEFAULT_CACHE_LIMIT;
        // Content cache gets a smaller budget (index.ts:243).
        let content_limit = std::cmp::max(64, limit / 5);
        Self {
            node_cache: LruCache::new(limit),
            file_cache: LruCache::new(content_limit),
            import_mapping_cache: LruCache::new(limit),
            re_export_cache: LruCache::new(limit),
            name_cache: LruCache::new(limit),
            lower_name_cache: LruCache::new(limit),
            qualified_name_cache: LruCache::new(limit),
            project_aliases: None,
            go_module: None,
            workspace_packages: None,
        }
    }
}

impl<'a> StoreResolutionContext<'a> {
    /// Build a context over `store` rooted at `project_root`.
    pub fn new(store: &'a Store, project_root: impl Into<String>) -> Self {
        Self {
            store,
            project_root: project_root.into(),
            caches: RefCell::new(Caches::new()),
        }
    }

    /// Drop all caches (`clearCaches`, `index.ts:313-324`). Called between
    /// resolution passes so the conformance pass sees freshly persisted edges.
    pub fn clear_caches(&self) {
        let mut c = self.caches.borrow_mut();
        c.node_cache.clear();
        c.file_cache.clear();
        c.import_mapping_cache.clear();
        c.re_export_cache.clear();
        c.name_cache.clear();
        c.lower_name_cache.clear();
        c.qualified_name_cache.clear();
    }
}

impl ResolutionContext for StoreResolutionContext<'_> {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.node_cache.get(&file_path.to_string()) {
            return cached;
        }
        let result = self.store.nodes_by_file_path(file_path).unwrap_or_default();
        c.node_cache.set(file_path.to_string(), result.clone());
        result
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.name_cache.get(&name.to_string()) {
            return cached;
        }
        let mut result = self.store.nodes_by_name(name).unwrap_or_default();
        order_candidates(&mut result);
        c.name_cache.set(name.to_string(), result.clone());
        result
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.qualified_name_cache.get(&qualified_name.to_string()) {
            return cached;
        }
        let mut result = self
            .store
            .nodes_by_qualified_name(qualified_name)
            .unwrap_or_default();
        order_candidates(&mut result);
        c.qualified_name_cache
            .set(qualified_name.to_string(), result.clone());
        result
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.store.nodes_by_kind(kind).unwrap_or_default()
    }

    fn known_node_names(&self) -> Vec<String> {
        self.store.all_node_names().unwrap_or_default()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        // Known-file fast path then filesystem fallback (index.ts:358-374).
        // The store is the index of known files.
        if self.store.file_by_path(file_path).ok().flatten().is_some() {
            return true;
        }
        let normalized = file_path.replace('\\', "/");
        if normalized != file_path
            && self
                .store
                .file_by_path(&normalized)
                .ok()
                .flatten()
                .is_some()
        {
            return true;
        }
        let full_path = Path::new(&self.project_root).join(file_path);
        full_path.exists()
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        let mut c = self.caches.borrow_mut();
        if c.file_cache.has(&file_path.to_string()) {
            return c.file_cache.get(&file_path.to_string()).flatten();
        }
        let full_path = Path::new(&self.project_root).join(file_path);
        let content = std::fs::read_to_string(&full_path).ok();
        c.file_cache.set(file_path.to_string(), content.clone());
        content
    }

    fn get_project_root(&self) -> &str {
        &self.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.store
            .all_files()
            .map(|files| files.into_iter().map(|f| f.path).collect())
            .unwrap_or_default()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.lower_name_cache.get(&lower_name.to_string()) {
            return cached;
        }
        let mut result = self
            .store
            .nodes_by_lower_name(lower_name)
            .unwrap_or_default();
        order_candidates(&mut result);
        c.lower_name_cache
            .set(lower_name.to_string(), result.clone());
        result
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.store.node_by_id(id).ok().flatten()
    }

    fn get_supertypes(&self, type_name: &str, language: Language) -> Vec<String> {
        // Union implements/extends targets of every same-named type node
        // (index.ts:425-442).
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
            for edge_kind in [EdgeKind::Implements, EdgeKind::Extends] {
                let edges = self
                    .store
                    .edges_by_source_kind(&tn.id, Some(edge_kind))
                    .unwrap_or_default();
                for edge in edges {
                    if let Ok(Some(target)) = self.store.node_by_id(&edge.target) {
                        if !target.name.is_empty() && target.name != type_name {
                            supertypes.insert(target.name);
                        }
                    }
                }
            }
        }
        supertypes.into_iter().collect()
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.import_mapping_cache.get(&file_path.to_string()) {
            return cached;
        }
        drop(c);
        let content = self.read_file(file_path);
        let mappings = match content {
            Some(text) => import_resolver::extract_import_mappings(&text, language),
            None => Vec::new(),
        };
        self.caches
            .borrow_mut()
            .import_mapping_cache
            .set(file_path.to_string(), mappings.clone());
        mappings
    }

    fn get_project_aliases(&self) -> Option<AliasMap> {
        let mut c = self.caches.borrow_mut();
        if c.project_aliases.is_none() {
            let loaded = load_project_aliases(&self.project_root);
            c.project_aliases = Some(loaded);
        }
        c.project_aliases.clone().flatten()
    }

    fn get_workspace_packages(&self) -> Option<WorkspacePackages> {
        let mut c = self.caches.borrow_mut();
        if c.workspace_packages.is_none() {
            let loaded = load_workspace_packages(&self.project_root);
            c.workspace_packages = Some(loaded);
        }
        c.workspace_packages.clone().flatten()
    }

    fn get_go_module(&self) -> Option<GoModule> {
        let mut c = self.caches.borrow_mut();
        if c.go_module.is_none() {
            c.go_module = Some(load_go_module(&self.project_root));
        }
        c.go_module.clone().flatten()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let mut c = self.caches.borrow_mut();
        if let Some(cached) = c.re_export_cache.get(&file_path.to_string()) {
            return cached;
        }
        drop(c);
        let content = self.read_file(file_path);
        // Re-key on the BARREL file's own extension (index.ts:489-498).
        let re_exports = match content {
            Some(text) => {
                let is_js_family = is_js_family_path(file_path);
                let lang = if is_js_family {
                    Language::TypeScript
                } else {
                    language
                };
                import_resolver::extract_re_exports(&text, lang)
            }
            None => Vec::new(),
        };
        self.caches
            .borrow_mut()
            .re_export_cache
            .set(file_path.to_string(), re_exports.clone());
        re_exports
    }
}

/// Shares the byte-equivalence-critical [`order_candidates`] tie-break with the
/// [`SnapshotResolutionContext`](crate::snapshot_context::SnapshotResolutionContext).
pub(crate) fn order_candidates_pub(nodes: &mut [Node]) {
    order_candidates(nodes);
}

/// Shares [`is_js_family_path`] with the snapshot context.
pub(crate) fn is_js_family_path_pub(file_path: &str) -> bool {
    is_js_family_path(file_path)
}

/// Shares [`load_go_module`] with the snapshot context.
pub(crate) fn load_go_module_pub(project_root: &str) -> Option<GoModule> {
    load_go_module(project_root)
}

/// Order name-lookup candidates by `(file_path, start_line, start_column, id)`.
///
/// A full index inserts nodes file-by-file in sorted-path order, each file's
/// nodes in `(start_line, start_column)` emission order, so a candidate list read
/// back in rowid (insertion) order already follows this key. The name matcher's
/// `find_best_match` tie-break keeps the FIRST candidate of an equal score, so
/// its result silently depends on that insertion order. An incremental sync
/// re-extracts a file and re-inserts its nodes at the table end, which would flip
/// such ties; sorting every candidate list by this stable key reproduces the
/// full-index order regardless of when a node was inserted, keeping incremental
/// resolution byte-equal to `index --force`.
fn order_candidates(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.start_column.cmp(&b.start_column))
            .then(a.id.cmp(&b.id))
    });
}

/// Matches `/\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?)$/i` (index.ts:496).
fn is_js_family_path(file_path: &str) -> bool {
    let lower = file_path.to_ascii_lowercase();
    lower.ends_with(".d.ts")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".cts")
        || lower.ends_with(".mts")
        || lower.ends_with(".ctsx")
        || lower.ends_with(".mtsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".cjs")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjsx")
        || lower.ends_with(".mjsx")
}

/// Load `module` path from `go.mod` at the project root
/// (mirrors `loadGoModule`, `upstream resolution/go-module.ts`).
fn load_go_module(project_root: &str) -> Option<GoModule> {
    let text = std::fs::read_to_string(pathutil::resolve(project_root, "go.mod")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("module") {
            let module_path = rest.trim();
            if !module_path.is_empty() {
                return Some(GoModule {
                    module_path: module_path.to_string(),
                });
            }
        }
    }
    None
}
