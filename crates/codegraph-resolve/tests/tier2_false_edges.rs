//! Tier 2 false-edge regression tests (upstream #1496 / #1566 / #1537 / #1536).
//!
//! A missing edge makes an agent look further; a FALSE edge makes it confidently
//! wrong. Each test here drives the FULL extract → resolve pipeline over a
//! committed fixture under `tests/fixtures/tier2/`, so it exercises real
//! tree-sitter extraction, real import-mapping extraction and real multi-file
//! resolution — behaviour a mock `ResolutionContext` structurally cannot reach.
//!
//! Fixtures are read in place and never written to; the store lives in a temp
//! file, so the worktree stays clean.

use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tier2")
        .join(name)
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-tier2-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
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

/// A resolved graph over one Tier 2 fixture: the store plus every non-`Contains`
/// edge the pipeline produced.
struct Resolved {
    store: Store,
    edges: Vec<Edge>,
}

impl Resolved {
    fn node(&self, id: &str) -> Node {
        self.store
            .node_by_id(id)
            .expect("node lookup")
            .expect("node exists")
    }

    fn of_kind(&self, kind: EdgeKind) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.kind == kind).collect()
    }

    /// `(source, target, line, resolvedBy)` for every edge of `kind`, rendered as
    /// `name@file:Lline` so a self-edge is distinguishable from a correct edge.
    fn described(&self, kind: EdgeKind) -> Vec<String> {
        let mut rows: Vec<String> = self
            .of_kind(kind)
            .into_iter()
            .map(|e| {
                let s = self.node(&e.source);
                let t = self.node(&e.target);
                let how = e
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("resolvedBy"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let conf = e
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("confidence"))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                format!(
                    "{:?}|{}@{}:L{} -> {:?}|{}@{}:L{} at {:?} [{how} {conf}]",
                    s.kind,
                    s.name,
                    s.file_path,
                    s.start_line,
                    t.kind,
                    t.name,
                    t.file_path,
                    t.start_line,
                    e.line,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    fn self_edges(&self, kind: EdgeKind) -> Vec<&Edge> {
        self.of_kind(kind)
            .into_iter()
            .filter(|e| e.source == e.target)
            .collect()
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

// ================= Item 4 — receiver preservation (#1496) ====================

#[test]
fn no_self_edge_for_member_receiver_call() {
    // Given three classes where `Outbox.send` and `Relay.forward` both call
    // `this.mailer.send(...)`,
    // When the pipeline resolves,
    // Then no `Calls` edge is a self-edge, and BOTH callers reach `Mailer::send`
    // (start_line 1) rather than the proximity-nearest `Outbox::send` at line 4.
    let g = resolve_fixture("item4");
    let described = g.described(EdgeKind::Calls);

    assert!(
        g.self_edges(EdgeKind::Calls).is_empty(),
        "a member-expression receiver must not resolve to the enclosing method: {described:?}"
    );

    let to_real_mailer = g
        .of_kind(EdgeKind::Calls)
        .into_iter()
        .filter(|e| {
            let t = g.node(&e.target);
            t.name == "send" && t.start_line == 1
        })
        .count();
    assert_eq!(
        to_real_mailer, 2,
        "both `this.mailer.send(m)` calls must target Mailer::send@L1: {described:?}"
    );

    assert_eq!(
        g.of_kind(EdgeKind::Calls).len(),
        2,
        "exactly the two repaired edges, no spurious extras: {described:?}"
    );
}

// ================= Item 5 — built-in receiver hijack (#1566) =================

/// `Calls` edges out of the symbol named `from`, described for assertion output.
fn calls_from(g: &Resolved, from: &str) -> Vec<String> {
    g.of_kind(EdgeKind::Calls)
        .into_iter()
        .filter(|e| g.node(&e.source).name == from)
        .map(|e| {
            let t = g.node(&e.target);
            format!("{}@{}:L{}", t.name, t.file_path, t.start_line)
        })
        .collect()
}

#[test]
fn builtin_receiver_does_not_bind_project_method() {
    // Given `useLocalMap`, whose receiver is a LOCAL built-in
    // `new Map<string, string>()`,
    // When resolution runs,
    // Then its three `set`/`get`/`has` calls bind to nothing — today each is
    // adopted by the unique project method at `instance-method 0.7`.
    let g = resolve_fixture("item5");
    let landed = calls_from(&g, "useLocalMap");
    assert!(
        landed.is_empty(),
        "a built-in Map receiver must not bind project methods, got: {landed:?}"
    );
}

#[test]
fn nested_builtin_receiver_stays_unresolved() {
    // `useNestedMap`'s receiver is `holder.values`, typed `Map<string, string>`
    // by the parameter annotation. Reachable only because item 4 now preserves
    // the `values` segment; item 5's gate is what refuses it.
    let g = resolve_fixture("item5");
    let landed = calls_from(&g, "useNestedMap");
    assert!(
        landed.is_empty(),
        "a nested built-in Map receiver must stay unresolved, got: {landed:?}"
    );
}

#[test]
fn class_field_builtin_receiver_is_refused() {
    // The WHOLE-fixture criterion. Every call in `item5` is a built-in `Map`
    // method — three via a class FIELD receiver, three via a local, one nested —
    // so the correct total is zero. A symbol-scoped assertion would pass while
    // the three `LRUCache::get/set/has → itself` self-edges survived, which is
    // exactly how that residue hid.
    let g = resolve_fixture("item5");
    let described = g.described(EdgeKind::Calls);
    assert!(
        g.of_kind(EdgeKind::Calls).is_empty(),
        "every call in item5 is a built-in Map method: {described:?}"
    );
    assert!(
        g.self_edges(EdgeKind::Calls).is_empty(),
        "and none may be a self-edge: {described:?}"
    );
}

#[test]
fn bound_project_map_resolves_through_both_import_routes() {
    // Item 5's false-refusal fence, over REAL import extraction: a project
    // `class Map` reached by a named import and by a barrel re-export must keep
    // resolving. The named/barrel discrimination is what makes this a fixture
    // rather than a mock — the predicate reads extracted import mappings.
    let g = resolve_fixture("bound");
    let described = g.described(EdgeKind::Calls);
    assert_eq!(
        g.of_kind(EdgeKind::Calls).len(),
        2,
        "both bound-Map calls must resolve: {described:?}"
    );
    let to_project_get = g
        .of_kind(EdgeKind::Calls)
        .into_iter()
        .filter(|e| {
            let t = g.node(&e.target);
            t.name == "get" && t.file_path == "other.ts"
        })
        .count();
    assert_eq!(
        to_project_get, 2,
        "both must target the project `Map::get` in other.ts: {described:?}"
    );
}

// ============ Item 6 — kind eligibility and import locality (#1537/#1536) ====

#[test]
fn phantom_import_binds_only_the_import_node() {
    // Given `import * as path from 'node:path'` and an unrelated class carrying a
    // `path` property,
    // When resolution runs,
    // Then the ONLY `imports` edge is the legitimate one to the `node:path`
    // import node. The second assertion is what stops an implementation from
    // passing by deleting all `imports` edges.
    let g = resolve_fixture("esm_import");
    let described = g.described(EdgeKind::Imports);
    let to_non_import = g
        .of_kind(EdgeKind::Imports)
        .into_iter()
        .filter(|e| g.node(&e.target).kind != NodeKind::Import)
        .count();
    assert_eq!(
        to_non_import, 0,
        "an out-of-repo import must not bind an in-repo symbol: {described:?}"
    );
    assert_eq!(
        g.of_kind(EdgeKind::Imports).len(),
        1,
        "and the edge to the import node itself must survive: {described:?}"
    );
}

#[test]
fn out_of_repo_supertype_does_not_bind_any_same_named_node() {
    // 6b + 6c together. `impl Error for MapperError` names the out-of-repo
    // `std::error::Error`; the fixture offers both an `enum_member Error` and a
    // sibling `type_alias Error`. Asserting zero `implements` edges to ANY
    // `Error`-named node is what fails a kind-filter-only implementation, which
    // would still bind the legal-kind type alias.
    let g = resolve_fixture("rust_supertype");
    let described = g.described(EdgeKind::Implements);
    let to_error = g
        .of_kind(EdgeKind::Implements)
        .into_iter()
        .filter(|e| g.node(&e.target).name == "Error")
        .count();
    assert_eq!(
        to_error, 0,
        "an out-of-repo supertype must bind nothing: {described:?}"
    );
}

#[test]
fn in_repo_rust_trait_implements_survives() {
    // 6d control — the real in-repo trait edge must be the ONLY `implements` edge,
    // and Gate 2 must not over-fire on the in-repo `use crate::ports::Sha256Port`
    // that binds it. Without the imports assertions an over-firing locality rule
    // would go unnoticed.
    let g = resolve_fixture("rust_supertype");
    let implements = g.described(EdgeKind::Implements);
    assert_eq!(
        g.of_kind(EdgeKind::Implements).len(),
        1,
        "exactly the in-repo trait edge: {implements:?}"
    );
    assert_eq!(
        g.node(&g.of_kind(EdgeKind::Implements)[0].target).name,
        "Sha256Port"
    );

    let imports = g.described(EdgeKind::Imports);
    assert_eq!(
        g.of_kind(EdgeKind::Imports).len(),
        3,
        "the in-repo `use` must keep resolving: {imports:?}"
    );
    let to_trait = g
        .of_kind(EdgeKind::Imports)
        .into_iter()
        .filter(|e| {
            let t = g.node(&e.target);
            t.kind == NodeKind::Trait && t.name == "Sha256Port"
        })
        .count();
    assert_eq!(
        to_trait, 1,
        "`use crate::ports::Sha256Port` must still bind the real trait: {imports:?}"
    );
}

#[test]
fn dropped_supertype_is_recorded_as_unresolved() {
    // The gates only ever REMOVE an edge; the reference stays recorded as failed,
    // which is the honest record for a supertype the repo does not contain.
    let g = resolve_fixture("rust_supertype");
    let recorded = g
        .store
        .all_unresolved_refs()
        .expect("unresolved refs")
        .into_iter()
        .filter(|r| r.reference_name.contains("Error"))
        .count();
    assert!(
        recorded >= 1,
        "the dropped `Error` supertype must remain in unresolved_refs"
    );
}

#[test]
fn npm_specifier_in_sfc_is_external() {
    // 6e — a Svelte `<script>` importing from an uninstalled npm package must not
    // have its supertype bound to a same-named project class. `is_external_import`
    // answered "not external" for SFC specifiers because its language guard
    // omitted them, even though import extraction already routes Svelte/Vue
    // through the JS path.
    let g = resolve_fixture("sfc");
    let described = g.described(EdgeKind::Implements);
    assert!(
        g.of_kind(EdgeKind::Implements).is_empty(),
        "an npm supertype in an SFC must bind nothing: {described:?}"
    );
}

#[test]
fn same_name_recursion_self_edge_survives() {
    // Given `Walker.run` recursing via `this.run(n - 1)` beside a same-named
    // `Other.run` (so the multi-candidate ranking branch is exercised),
    // When the pipeline resolves,
    // Then the self-edge is KEPT — it is the correct target. This is the
    // falsifier for the rejected blanket self-exclusion design.
    let g = resolve_fixture("recur");
    let described = g.described(EdgeKind::Calls);
    assert_eq!(
        g.self_edges(EdgeKind::Calls).len(),
        1,
        "the legitimate recursion self-edge must survive: {described:?}"
    );
    assert_eq!(
        g.of_kind(EdgeKind::Calls).len(),
        1,
        "and it must be the only calls edge: {described:?}"
    );
}
