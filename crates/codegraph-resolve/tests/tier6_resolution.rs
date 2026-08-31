//! Tier 6 resolution regression tests (upstream #1518/#1453, #1521, #1510/PR #1511).
//!
//! Each test drives the FULL extract → resolve pipeline over a committed fixture
//! under `tests/fixtures/tier6/`, so it exercises real tree-sitter extraction,
//! real import-mapping extraction and real multi-file resolution — behaviour a
//! mock `ResolutionContext` structurally cannot reach.
//!
//! The three cases are INDEPENDENT code paths and therefore separate fixtures,
//! so an isolated revert of one fix cannot be masked by another:
//!   * `py_from_import` — `import_resolver.rs` Python `from`-import member
//!     resolution (#1518). A test that only asserted "an edge exists" would pass
//!     with the edge still bound to the DECOY, so every assertion here names the
//!     TARGET FILE.
//!   * `go_multi_module` — `context.rs` multi-`go.mod` module discovery (#1521).
//!   * `ts_decl_initializer` — `walker.rs` declaration-initializer attribution
//!     (#1510). Asserts the SOURCE of the edge is the declared symbol, not the
//!     file node.
//!
//! Fixtures are read in place and never written to; the store lives in a temp
//! file, so the worktree stays clean.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

static NEXT_TEMP_DB: AtomicU64 = AtomicU64::new(0);

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tier6")
        .join(name)
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    temp_db_path_at(test_name, nanos)
}

fn temp_db_path_at(test_name: &str, nanos: u128) -> PathBuf {
    let mut path = std::env::temp_dir();
    // Windows may return the same wall-clock value to parallel tests. The
    // process-local nonce keeps same-fixture stores collision-free regardless
    // of clock resolution.
    let nonce = NEXT_TEMP_DB.fetch_add(1, Ordering::Relaxed);
    path.push(format!(
        "codegraph-tier6-{test_name}-{}-{nanos}-{nonce}.db",
        std::process::id()
    ));
    path
}

#[test]
fn temp_db_paths_remain_unique_when_clock_does_not_advance() {
    assert_ne!(
        temp_db_path_at("same_fixture", 42),
        temp_db_path_at("same_fixture", 42)
    );
}

/// Every indexable file under `root`, as project-relative `/`-separated paths,
/// sorted so indexing order is deterministic.
fn fixture_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).expect("read fixture dir");
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// A resolved graph over one Tier 6 fixture: the store plus every non-`Contains`
/// edge the pipeline produced.
struct Resolved {
    store: Store,
    edges: Vec<Edge>,
}

impl Resolved {
    fn node(&self, id: &str) -> Option<Node> {
        self.store.node_by_id(id).expect("node lookup")
    }

    /// `name@file` for a node id, or the raw id when it is not a node (a `file:`
    /// id is a node too, so this only degrades for a dangling id).
    fn label(&self, id: &str) -> String {
        match self.node(id) {
            Some(n) => format!("{:?}|{}@{}:L{}", n.kind, n.name, n.file_path, n.start_line),
            None => id.to_string(),
        }
    }

    fn of_kind(&self, kind: EdgeKind) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.kind == kind).collect()
    }

    /// `source -> target at line` for every edge of `kind`, sorted.
    fn described(&self, kind: EdgeKind) -> Vec<String> {
        let mut rows: Vec<String> = self
            .of_kind(kind)
            .into_iter()
            .map(|e| {
                format!(
                    "{} -> {} at {:?}",
                    self.label(&e.source),
                    self.label(&e.target),
                    e.line
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// The FILE PATHS of every `kind` edge whose source node is named
    /// `source_name` and whose target node is named `target_name`. Naming the
    /// target's file is the whole point for #1518: an edge to the decoy and an
    /// edge to the real target are both "an edge to `real_target`".
    fn target_files(&self, kind: EdgeKind, source_name: &str, target_name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .of_kind(kind)
            .into_iter()
            .filter_map(|e| {
                let s = self.node(&e.source)?;
                let t = self.node(&e.target)?;
                (s.name == source_name && t.name == target_name).then_some(t.file_path)
            })
            .collect();
        out.sort();
        out
    }
}

fn resolve_fixture(name: &str) -> Resolved {
    let root = fixture_root(name);
    assert!(root.is_dir(), "missing committed fixture {name}");
    let relative = fixture_files(&root);
    assert!(!relative.is_empty(), "fixture {name} has no files");

    let mut store = Store::open(&temp_db_path(name)).expect("open store");
    for path in &relative {
        let language = detect_language(path);
        let result = extract_file(&root, path).expect("extract file");
        store
            .upsert_file(&FileRecord {
                path: path.clone(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
                generated: false,
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store
            .insert_edges(&result.edges)
            .expect("insert contains edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }

    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    {
        let context = codegraph_resolve::StoreResolutionContext::new(&store, &root_str);
        resolver.initialize(&context);
    }
    resolver
        .extract_and_persist_frameworks(&mut store, &relative)
        .expect("framework extract");
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");
    resolver
        .run_post_extract(&mut store)
        .expect("run post extract");

    let mut edges = Vec::new();
    for kind in NodeKind::ALL {
        for node in store.nodes_by_kind(kind).expect("nodes by kind") {
            for edge in store
                .edges_by_source_kind(&node.id, None)
                .expect("edges by source")
            {
                if edge.kind != EdgeKind::Contains {
                    edges.push(edge);
                }
            }
        }
    }
    Resolved { store, edges }
}

// ============ Item 20 — Python `from`-import member resolution (#1518) ========

#[test]
fn python_from_import_binds_to_the_imported_module_not_a_decoy() {
    // Given `pkg/mod.py` and `pkg/decoy.py` BOTH defining `real_target`, and
    // `app.py` doing `from pkg.mod import real_target`,
    // When the pipeline resolves `caller`'s call,
    // Then the edge must target the `real_target` in `pkg/mod.py` — the file the
    // import explicitly names. Before the fix it bound to `pkg/decoy.py`, which
    // an "edge exists" assertion would have accepted.
    let g = resolve_fixture("py_from_import");
    let described = g.described(EdgeKind::Calls);

    assert_eq!(
        g.target_files(EdgeKind::Calls, "caller", "real_target"),
        vec!["pkg/mod.py".to_string()],
        "`from pkg.mod import real_target` must bind to pkg/mod.py, not the \
         same-named decoy: {described:?}"
    );
}

#[test]
fn python_from_import_alias_binds_to_the_imported_module() {
    // The `as aliased` form emitted NO call edge at all before the fix, so this
    // asserts both that an edge exists AND that it names pkg/mod.py.
    let g = resolve_fixture("py_from_import");
    let described = g.described(EdgeKind::Calls);

    assert_eq!(
        g.target_files(EdgeKind::Calls, "caller2", "real_target"),
        vec!["pkg/mod.py".to_string()],
        "`from pkg.mod import real_target as aliased` must bind the aliased call \
         to pkg/mod.py: {described:?}"
    );
}

#[test]
fn python_from_import_never_reaches_the_decoy() {
    // The negative half: no `Calls` edge anywhere in the fixture may land in
    // `pkg/decoy.py`. Without this, a fix that ADDED the correct edge while
    // KEEPING the wrong one would pass the two tests above.
    let g = resolve_fixture("py_from_import");
    let to_decoy: Vec<String> = g
        .of_kind(EdgeKind::Calls)
        .into_iter()
        .filter(|e| {
            g.node(&e.target)
                .is_some_and(|t| t.file_path == "pkg/decoy.py")
        })
        .map(|e| format!("{} -> {}", g.label(&e.source), g.label(&e.target)))
        .collect();
    assert!(
        to_decoy.is_empty(),
        "no call may reach the decoy module: {to_decoy:?}"
    );
}

// ============ Item 21 — Go cross-module calls (#1521) =========================

#[test]
fn go_cross_module_call_resolves_in_multi_go_mod_layout() {
    // Given two modules (`a/go.mod` = `ex/a`, `b/go.mod` = `ex/b`) with NO root
    // `go.mod`, and `b/main.go` calling `a.Helper()` after `import "ex/a"`,
    // When the pipeline resolves,
    // Then a `Calls` edge must reach `Helper` in `a/helper.go`. Before the fix
    // the whole fixture produced ZERO `calls` edges, because module discovery
    // only ever read `<project_root>/go.mod`.
    let g = resolve_fixture("go_multi_module");
    let described = g.described(EdgeKind::Calls);

    assert_eq!(
        g.target_files(EdgeKind::Calls, "Caller", "Helper"),
        vec!["a/helper.go".to_string()],
        "`a.Helper()` across two modules must reach a/helper.go: {described:?}"
    );
}

#[test]
fn go_cross_module_call_does_not_bind_the_same_named_decoy() {
    // `b/local.go` defines its own exported `Helper`. The import names module
    // `ex/a`, so the edge must NOT land on the caller's own package.
    let g = resolve_fixture("go_multi_module");
    let described = g.described(EdgeKind::Calls);
    let files = g.target_files(EdgeKind::Calls, "Caller", "Helper");
    assert!(
        !files.contains(&"b/local.go".to_string()),
        "the in-package same-named Helper must not win over the imported \
         module's: {described:?}"
    );
}

// ============ Item 22 — declaration initializers (#1510 / PR #1511) ==========

#[test]
fn ts_top_level_declaration_initializer_call_attributes_to_the_declaration() {
    // Given `export const value = target();` at file scope,
    // When the pipeline resolves,
    // Then the `Calls` edge SOURCE must be the `value` constant, not `file:d.ts`
    // — otherwise `codegraph callers target` answers with a filename.
    let g = resolve_fixture("ts_decl_initializer");
    let described = g.described(EdgeKind::Calls);

    let sources: Vec<String> = g
        .of_kind(EdgeKind::Calls)
        .into_iter()
        .filter(|e| g.node(&e.target).is_some_and(|t| t.name == "target"))
        .map(|e| {
            let s = g.node(&e.source).expect("source node exists");
            format!("{:?}|{}", s.kind, s.name)
        })
        .collect();

    assert!(
        sources.contains(&"Constant|value".to_string()),
        "the initializer call must attribute to the `value` constant: \
         sources={sources:?} edges={described:?}"
    );
    assert!(
        !sources.iter().any(|s| s.starts_with("File|")),
        "no initializer call may still attribute to the file node: \
         sources={sources:?} edges={described:?}"
    );
}

#[test]
fn ts_declaration_initializer_fix_leaves_function_local_calls_alone() {
    // The control: a `const inner = target()` INSIDE a function already
    // attributes to that function. A fix that pushed the declaration node too
    // eagerly would re-point this edge at a `constant:inner` node, so this test
    // fails if the change leaks past file scope.
    let g = resolve_fixture("ts_decl_initializer");
    let described = g.described(EdgeKind::Calls);

    let from_wrapper: Vec<String> = g
        .of_kind(EdgeKind::Calls)
        .into_iter()
        .filter_map(|e| {
            let s = g.node(&e.source)?;
            (s.name == "wrapper").then(|| self_label(&s))
        })
        .collect();
    assert_eq!(
        from_wrapper.len(),
        1,
        "the function-local `const inner = target()` must stay attributed to \
         `wrapper`: {described:?}"
    );
}

fn self_label(n: &Node) -> String {
    format!("{:?}|{}", n.kind, n.name)
}
