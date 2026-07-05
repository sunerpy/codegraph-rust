//! Store-backed [`ResolutionContext`].
//!
//! Ports the SQLite-backed context created in
//! `upstream resolution/index.ts:329-505` (the `createContext`
//! closure). Node lookups go through the indexed [`Store`] queries (ported in
//! Task 10, `db/queries.ts`); per-call results are memoised through the same
//! per-resolver LRU caches the upstream uses (`index.ts:217-250`). Resolution is sync.

use crate::lru_cache::LruCache;
use crate::path_aliases::{AliasMap, load_project_aliases};
use crate::types::{GoModule, ImportMapping, ReExport, ResolutionContext};
use crate::workspace_packages::{WorkspacePackages, load_workspace_packages};
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

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Edge, FileRecord, NodeKind};

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("cg-ctx-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).expect("mkdir");
        p
    }

    fn open_store(root: &std::path::Path) -> Store {
        Store::open(&root.join("index.db")).expect("open store")
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
        }
    }

    #[test]
    fn is_js_family_path_matches_extensions() {
        for p in [
            "a.ts", "a.tsx", "a.d.ts", "a.cts", "a.mts", "a.js", "a.jsx", "a.cjs", "a.mjs",
        ] {
            assert!(is_js_family_path(p), "{p} should be js-family");
        }
        assert!(is_js_family_path("A.TS"));
        assert!(!is_js_family_path("a.py"));
        assert!(!is_js_family_path("a.rs"));
    }

    #[test]
    fn order_candidates_sorts_by_file_line_col_id() {
        let mut nodes = vec![
            node("z", "n", NodeKind::Function, "b.ts", 5, 0),
            node("a", "n", NodeKind::Function, "a.ts", 2, 3),
            node("b", "n", NodeKind::Function, "a.ts", 2, 1),
            node("c", "n", NodeKind::Function, "a.ts", 1, 0),
        ];
        order_candidates(&mut nodes);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b", "a", "z"]);
    }

    #[test]
    fn load_go_module_reads_module_line() {
        let root = temp_root("gomod");
        std::fs::write(root.join("go.mod"), "module example.com/proj\n\ngo 1.22\n").unwrap();
        let gm = load_go_module(root.to_str().unwrap()).expect("go module");
        assert_eq!(gm.module_path, "example.com/proj");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_go_module_none_when_absent_or_empty() {
        let root = temp_root("nogomod");
        assert!(load_go_module(root.to_str().unwrap()).is_none());
        std::fs::write(root.join("go.mod"), "module\n").unwrap();
        assert!(load_go_module(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_context_node_lookups_and_caching() {
        let root = temp_root("lookups");
        let mut store = open_store(&root);
        store.upsert_file(&file_record("a.ts", 2)).unwrap();
        store
            .upsert_nodes(&[
                node("f1", "foo", NodeKind::Function, "a.ts", 1, 0),
                node("c1", "Widget", NodeKind::Class, "a.ts", 5, 0),
            ])
            .unwrap();

        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        assert_eq!(ctx.get_nodes_in_file("a.ts").len(), 2);
        // Second call hits the cache path.
        assert_eq!(ctx.get_nodes_in_file("a.ts").len(), 2);
        assert_eq!(ctx.get_nodes_by_name("foo").len(), 1);
        assert_eq!(ctx.get_nodes_by_name("foo").len(), 1);
        assert_eq!(ctx.get_nodes_by_qualified_name("Widget").len(), 1);
        assert_eq!(ctx.get_nodes_by_qualified_name("Widget").len(), 1);
        assert_eq!(ctx.get_nodes_by_kind(NodeKind::Function).len(), 1);
        assert_eq!(ctx.get_nodes_by_lower_name("foo").len(), 1);
        assert_eq!(ctx.get_nodes_by_lower_name("foo").len(), 1);
        assert!(ctx.get_node_by_id("f1").is_some());
        assert!(ctx.get_node_by_id("missing").is_none());
        assert!(ctx.known_node_names().contains(&"foo".to_string()));
        assert_eq!(ctx.get_project_root(), root.to_str().unwrap());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_context_file_exists_and_read_file() {
        let root = temp_root("files");
        let store = open_store(&root);
        store.upsert_file(&file_record("known.ts", 0)).unwrap();
        std::fs::write(root.join("ondisk.ts"), "export const x = 1;\n").unwrap();

        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        assert!(ctx.file_exists("known.ts"));
        assert!(ctx.file_exists("ondisk.ts"));
        assert!(!ctx.file_exists("nowhere.ts"));

        let content = ctx.read_file("ondisk.ts").expect("read");
        assert!(content.contains("export const x"));
        // Cached second read.
        assert!(ctx.read_file("ondisk.ts").is_some());
        assert!(ctx.read_file("missing.ts").is_none());
        assert!(ctx.read_file("missing.ts").is_none());

        assert!(ctx.get_all_files().contains(&"known.ts".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_context_import_mappings_and_re_exports() {
        let root = temp_root("imports");
        let store = open_store(&root);
        std::fs::write(
            root.join("a.ts"),
            "import { foo } from './b';\nexport { foo } from './b';\nexport * from './c';\n",
        )
        .unwrap();
        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        let mappings = ctx.get_import_mappings("a.ts", Language::TypeScript);
        assert!(!mappings.is_empty());
        // Cached second call.
        assert!(
            !ctx.get_import_mappings("a.ts", Language::TypeScript)
                .is_empty()
        );
        let re = ctx.get_re_exports("a.ts", Language::TypeScript);
        assert!(!re.is_empty());
        assert!(!ctx.get_re_exports("a.ts", Language::TypeScript).is_empty());
        // Missing file yields empty.
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
    fn store_context_project_config_caches() {
        let root = temp_root("config");
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{ "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("packages/ui")).unwrap();
        std::fs::write(
            root.join("packages/ui/package.json"),
            r#"{ "name": "@app/ui" }"#,
        )
        .unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/x\n").unwrap();

        let store = open_store(&root);
        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        assert!(ctx.get_project_aliases().is_some());
        assert!(ctx.get_project_aliases().is_some());
        assert!(ctx.get_workspace_packages().is_some());
        assert!(ctx.get_workspace_packages().is_some());
        assert!(ctx.get_go_module().is_some());
        assert!(ctx.get_go_module().is_some());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_context_get_supertypes_reads_edges() {
        let root = temp_root("supertypes");
        let mut store = open_store(&root);
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

        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        let supers = ctx.get_supertypes("Child", Language::TypeScript);
        assert_eq!(supers, vec!["Base".to_string()]);
        // Unknown type yields empty.
        assert!(ctx.get_supertypes("Nope", Language::TypeScript).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_context_clear_caches_resets() {
        let root = temp_root("clear");
        let mut store = open_store(&root);
        store.upsert_file(&file_record("a.ts", 1)).unwrap();
        store
            .upsert_nodes(&[node("f1", "foo", NodeKind::Function, "a.ts", 1, 0)])
            .unwrap();
        let ctx = StoreResolutionContext::new(&store, root.to_str().unwrap());
        assert_eq!(ctx.get_nodes_by_name("foo").len(), 1);
        ctx.clear_caches();
        // Still correct after clearing (re-reads from store).
        assert_eq!(ctx.get_nodes_by_name("foo").len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pub_crate_helpers_delegate() {
        let mut nodes = vec![
            node("b", "n", NodeKind::Function, "b.ts", 1, 0),
            node("a", "n", NodeKind::Function, "a.ts", 1, 0),
        ];
        order_candidates_pub(&mut nodes);
        assert_eq!(nodes[0].id, "a");
        assert!(is_js_family_path_pub("x.ts"));
        assert!(!is_js_family_path_pub("x.py"));

        let root = temp_root("helper-go");
        std::fs::write(root.join("go.mod"), "module m/n\n").unwrap();
        assert_eq!(
            load_go_module_pub(root.to_str().unwrap()).map(|g| g.module_path),
            Some("m/n".to_string())
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
