//! The resolution context the link passes read their routers' tables and
//! hrefs through (W7-18a). A router's table, its href readers and import
//! resolution take a [`ResolutionContext`]; [`View::context`] gives them one
//! over the stage's view that reads what a pass may read. Each method of the
//! trait is in one of three groups:
//!
//! - **text, from the view**: [`ResolutionContext::read_file`],
//!   `read_file_shared`, `is_file_readable` and `source_facts` read an
//!   indexed file's [`View::text`], absent for a file the index does not hold
//!   or one that drifted since it was indexed (W7-14's rule);
//!   `get_import_mappings` and `get_re_exports` extract from that text as
//!   [`StoreResolutionContext`] extracts from the file, a barrel's
//!   re-exports in its own file's language; `route_tables` is the stage
//!   run's, so each router's table is built once a run, through this
//!   context;
//! - **edges and class heads, none**: `get_supertypes` and `recorded_heads`
//!   keep the trait's defaults, so no edge is read here and a pass reads
//!   edges only through the view;
//! - **everything else, from the store context**: node, name, kind and id
//!   lookups, the file list, `file_exists` and `is_indexed_file` (the files
//!   table, then a probe on disk within the root), the per-file callables
//!   and methods, every language's tables and the Go, Rust and C++ module
//!   readers, and the configuration loaders, which read the project's
//!   `tsconfig.json` / `jsconfig.json` aliases, workspace packages and
//!   `package.json` manifests from disk within the root as resolution does.
//!   Those files are JavaScript's control inputs (`control_inputs.rs`): when
//!   one changes, the JavaScript class is resolved again and the stage's
//!   marker withdrawn, so the stage reads the configuration resolution read.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use codegraph_core::types::{Language, Node, NodeKind};

use super::stage::View;
use crate::context::{StoreResolutionContext, is_js_family_path_pub};
use crate::frameworks::route_href::RouteTables;
use crate::import_resolver::{extract_import_mappings, extract_re_exports};
use crate::name_matcher::{FileCallables, FileMethods, NameMatcherTables, PatternCheck};
use crate::package_manifests::ManifestScope;
use crate::path_aliases::AliasMap;
use crate::source_facts::SourceFacts;
use crate::types::{GoModule, ImportMapping, ReExport, ResolutionContext};
use crate::workspace_packages::WorkspacePackages;

/// [`View`] as a [`ResolutionContext`]: see the module documentation.
pub(super) struct ViewContext<'v, 'a> {
    view: &'v View<'a>,
    store: StoreResolutionContext<'a>,
    imports: RefCell<HashMap<(String, Language), Vec<ImportMapping>>>,
    re_exports: RefCell<HashMap<(String, Language), Vec<ReExport>>>,
    facts: RefCell<HashMap<String, Option<Arc<SourceFacts>>>>,
}

impl<'v, 'a> ViewContext<'v, 'a> {
    /// The context over `view`, whose base graph `store` holds and whose
    /// files are read under `max_file_size` ([`View::context`]).
    pub(super) fn new(view: &'v View<'a>, store: StoreResolutionContext<'a>) -> Self {
        Self {
            view,
            store,
            imports: RefCell::new(HashMap::new()),
            re_exports: RefCell::new(HashMap::new()),
            facts: RefCell::new(HashMap::new()),
        }
    }
}

impl ResolutionContext for ViewContext<'_, '_> {
    // ---- Text, from the view ----

    fn read_file(&self, file_path: &str) -> Option<String> {
        self.read_file_shared(file_path)
            .map(|text| text.to_string())
    }

    fn is_file_readable(&self, file_path: &str) -> bool {
        self.read_file_shared(file_path).is_some()
    }

    fn read_file_shared(&self, file_path: &str) -> Option<Arc<str>> {
        self.view.text(&file_path.replace('\\', "/"))
    }

    fn source_facts(&self, file_path: &str) -> Option<Arc<SourceFacts>> {
        if let Some(facts) = self.facts.borrow().get(file_path) {
            return facts.clone();
        }
        let facts = self
            .read_file_shared(file_path)
            .map(|source| Arc::new(SourceFacts::new(source)));
        self.facts
            .borrow_mut()
            .insert(file_path.to_string(), facts.clone());
        facts
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        let key = (file_path.to_string(), language);
        if let Some(mappings) = self.imports.borrow().get(&key) {
            return mappings.clone();
        }
        let mappings = self
            .read_file_shared(file_path)
            .map(|text| extract_import_mappings(&text, language))
            .unwrap_or_default();
        self.imports.borrow_mut().insert(key, mappings.clone());
        mappings
    }

    /// Read as [`StoreResolutionContext`] reads it: in the barrel file's own
    /// language, TypeScript for every file of the JavaScript family.
    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let language = if is_js_family_path_pub(file_path) {
            Language::TypeScript
        } else {
            language
        };
        let key = (file_path.to_string(), language);
        if let Some(re_exports) = self.re_exports.borrow().get(&key) {
            return re_exports.clone();
        }
        let re_exports = self
            .read_file_shared(file_path)
            .map(|text| extract_re_exports(&text, language))
            .unwrap_or_default();
        self.re_exports.borrow_mut().insert(key, re_exports.clone());
        re_exports
    }

    fn route_tables(&self) -> Arc<RouteTables> {
        self.view.route_tables()
    }

    // ---- Edges and class heads: `get_supertypes` and `recorded_heads`
    // keep the trait's defaults ----

    // ---- Everything else, from the store context ----

    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.store.get_nodes_in_file(file_path)
    }

    fn get_nodes_in_file_shared(&self, file_path: &str) -> Vec<Arc<Node>> {
        self.store.get_nodes_in_file_shared(file_path)
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.store.get_nodes_by_name(name)
    }

    fn get_nodes_by_name_shared(&self, name: &str) -> Vec<Arc<Node>> {
        self.store.get_nodes_by_name_shared(name)
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.store.get_nodes_by_qualified_name(qualified_name)
    }

    fn get_nodes_by_qualified_name_shared(&self, qualified_name: &str) -> Vec<Arc<Node>> {
        self.store
            .get_nodes_by_qualified_name_shared(qualified_name)
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.store.get_nodes_by_kind(kind)
    }

    fn get_nodes_by_kind_shared(&self, kind: NodeKind) -> Vec<Arc<Node>> {
        self.store.get_nodes_by_kind_shared(kind)
    }

    fn known_node_names(&self) -> Vec<String> {
        self.store.known_node_names()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        self.store.file_exists(file_path)
    }

    fn is_indexed_file(&self, file_path: &str) -> bool {
        self.store.is_indexed_file(file_path)
    }

    fn file_has_exported_node(&self, file_path: &str) -> bool {
        self.store.file_has_exported_node(file_path)
    }

    fn declared_dependencies(&self) -> Arc<BTreeSet<String>> {
        self.store.declared_dependencies()
    }

    fn get_project_root(&self) -> &str {
        self.store.get_project_root()
    }

    fn get_all_files(&self) -> Vec<String> {
        self.store.get_all_files()
    }

    fn get_all_files_shared(&self) -> Arc<Vec<String>> {
        self.store.get_all_files_shared()
    }

    fn get_files_by_basename_shared(&self, basename: &str) -> Arc<Vec<String>> {
        self.store.get_files_by_basename_shared(basename)
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.store.get_nodes_by_lower_name(lower_name)
    }

    fn get_nodes_by_lower_name_shared(&self, lower_name: &str) -> Vec<Arc<Node>> {
        self.store.get_nodes_by_lower_name_shared(lower_name)
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.store.get_node_by_id(id)
    }

    fn get_node_by_id_shared(&self, id: &str) -> Option<Arc<Node>> {
        self.store.get_node_by_id_shared(id)
    }

    fn get_project_aliases(&self) -> Option<AliasMap> {
        self.store.get_project_aliases()
    }

    fn get_nearest_aliases(&self, from_file: &str) -> Option<AliasMap> {
        self.store.get_nearest_aliases(from_file)
    }

    fn get_workspace_packages(&self) -> Option<WorkspacePackages> {
        self.store.get_workspace_packages()
    }

    fn manifest_scope(&self, dir: &str) -> Arc<ManifestScope> {
        self.store.manifest_scope(dir)
    }

    fn get_go_module(&self) -> Option<GoModule> {
        self.store.get_go_module()
    }

    fn get_go_modules(&self) -> Vec<GoModule> {
        self.store.get_go_modules()
    }

    fn get_rust_crates(&self) -> Arc<crate::rust_scope::RustCrates> {
        self.store.get_rust_crates()
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.store.get_cpp_include_dirs()
    }

    fn python_write_index(&self) -> Arc<crate::python_globals::PythonWriteIndex> {
        self.store.python_write_index()
    }

    fn cpp_namespace_tables(&self) -> Arc<crate::cpp_namespaces::CppNamespaceTables> {
        self.store.cpp_namespace_tables()
    }

    fn kotlin_tables(&self) -> Arc<crate::kotlin_scope::KotlinTables> {
        self.store.kotlin_tables()
    }

    fn spring_tables(&self) -> Arc<crate::frameworks::spring::SpringTables> {
        self.store.spring_tables()
    }

    fn scala_tables(&self) -> Arc<crate::scala_scope::ScalaTables> {
        self.store.scala_tables()
    }

    fn csharp_tables(&self) -> Arc<crate::csharp_types::CsharpTables> {
        self.store.csharp_tables()
    }

    fn member_tables(&self) -> Arc<crate::member_types::MemberTables> {
        self.store.member_tables()
    }

    fn ruby_tables(&self) -> Arc<crate::ruby_scope::RubyTables> {
        self.store.ruby_tables()
    }

    fn php_tables(&self) -> Arc<crate::php_scope::PhpTables> {
        self.store.php_tables()
    }

    fn lua_tables(&self) -> Arc<crate::lua_scope::LuaTables> {
        self.store.lua_tables()
    }

    fn dart_tables(&self) -> Arc<crate::dart_scope::DartTables> {
        self.store.dart_tables()
    }

    fn dart_libraries(&self) -> Arc<crate::dart_libraries::DartLibraries> {
        self.store.dart_libraries()
    }

    fn compiled_patterns(
        &self,
        key: &str,
        build: &dyn Fn() -> Vec<(regex::Regex, PatternCheck)>,
    ) -> Arc<Vec<(regex::Regex, PatternCheck)>> {
        self.store.compiled_patterns(key, build)
    }

    fn cfml_tables(&self) -> Arc<crate::cfml_scope::CfmlTables> {
        self.store.cfml_tables()
    }

    fn swift_tables(&self) -> Arc<crate::swift_type_visibility::SwiftTables> {
        self.store.swift_tables()
    }

    fn objc_tables(&self) -> Arc<crate::objc_scope::ObjcTables> {
        self.store.objc_tables()
    }

    fn go_tables(&self) -> Arc<crate::go_packages::GoTables> {
        self.store.go_tables()
    }

    fn solidity_tables(&self) -> Arc<crate::solidity_scope::SolidityTables> {
        self.store.solidity_tables()
    }

    fn enclosing_function_lines(
        &self,
        file_path: &str,
        qualified: &str,
        start: i64,
        end: i64,
    ) -> Option<(i64, i64)> {
        self.store
            .enclosing_function_lines(file_path, qualified, start, end)
    }

    fn file_callables(&self, file_path: &str) -> Arc<FileCallables> {
        self.store.file_callables(file_path)
    }

    fn file_methods(&self, file_path: &str) -> Arc<FileMethods> {
        self.store.file_methods(file_path)
    }

    fn has_callable_named(&self, qualified: &str) -> bool {
        self.store.has_callable_named(qualified)
    }

    fn method_files(&self, name: &str) -> Option<Arc<HashSet<String>>> {
        self.store.method_files(name)
    }

    fn name_matcher_tables(&self) -> Arc<NameMatcherTables> {
        self.store.name_matcher_tables()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind};
    use codegraph_store::Store;

    use super::super::stage::View;
    use crate::types::ResolutionContext;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-view-context-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `path` indexed with `indexed` as its content, its file now holding
    /// `on_disk`.
    fn index_file(store: &Store, dir: &std::path::Path, path: &str, indexed: &str, on_disk: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, on_disk).unwrap();
        store
            .upsert_file(&FileRecord {
                path: path.to_string(),
                content_hash: hash_content(indexed),
                language: Language::TypeScript,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: 0,
                errors: Vec::new(),
                generated: false,
            })
            .unwrap();
    }

    fn class(name: &str, file: &str) -> Node {
        Node {
            id: format!("class:{file}:{name}"),
            kind: NodeKind::Class,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// A file that drifted since it was indexed reads as absent, its imports
    /// and re-exports as none; a file the index does not hold has no text;
    /// a file as it was indexed reads.
    #[test]
    fn a_drifted_or_unindexed_file_reads_as_absent() {
        let dir = temp_dir("drift");
        let store = Store::open(&dir.join("index.db")).unwrap();
        let barrel = "import { a } from './a';\nexport { b } from './b';\n";
        index_file(&store, &dir, "src/fresh.ts", barrel, barrel);
        index_file(
            &store,
            &dir,
            "src/drifted.ts",
            barrel,
            "import { c } from './c';\nexport { d } from './d';\n",
        );
        std::fs::write(dir.join("src/unindexed.ts"), barrel).unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let context = view.context();
        assert_eq!(context.read_file("src/fresh.ts").as_deref(), Some(barrel));
        assert_eq!(
            context
                .get_import_mappings("src/fresh.ts", Language::TypeScript)
                .len(),
            1
        );
        assert_eq!(
            context
                .get_re_exports("src/fresh.ts", Language::TypeScript)
                .len(),
            1
        );
        for path in ["src/drifted.ts", "src/unindexed.ts"] {
            assert_eq!(context.read_file(path), None, "{path}");
            assert!(!context.is_file_readable(path), "{path}");
            assert!(context.source_facts(path).is_none(), "{path}");
            assert!(
                context
                    .get_import_mappings(path, Language::TypeScript)
                    .is_empty(),
                "{path}"
            );
            assert!(
                context
                    .get_re_exports(path, Language::TypeScript)
                    .is_empty(),
                "{path}"
            );
        }
        drop(context);
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The context reads no edge: a class's supertypes are none, though the
    /// base graph holds its `extends` edge, and no class-head row is read.
    #[test]
    fn the_context_reads_no_edge() {
        let dir = temp_dir("edges");
        let mut store = Store::open(&dir.join("index.db")).unwrap();
        let (base, derived) = (class("Base", "src/a.ts"), class("Derived", "src/a.ts"));
        store
            .upsert_nodes(&[base.clone(), derived.clone()])
            .unwrap();
        store
            .insert_edges(&[Edge {
                id: None,
                source: derived.id.clone(),
                target: base.id.clone(),
                kind: EdgeKind::Extends,
                metadata: None,
                line: Some(1),
                col: Some(0),
                provenance: None,
            }])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let context = view.context();
        assert!(
            context
                .get_supertypes("Derived", Language::TypeScript)
                .is_empty()
        );
        assert!(context.recorded_heads(&derived).is_none());
        drop(context);
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
