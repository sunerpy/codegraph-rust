//! Differential equivalence: [`SnapshotResolutionContext`] must produce
//! byte-identical resolution to [`StoreResolutionContext`] over a non-trivial graph.
//!
//! Builds a representative multi-language project (import mappings, re-exports,
//! qualified-name resolution, and supertype/conformance resolution), indexes it
//! into a real [`Store`], runs the store-backed resolver once so `implements`/
//! `extends` edges exist, then builds BOTH contexts and asserts that
//! [`ReferenceResolver::resolve_one`] returns an identical [`ResolvedRef`]
//! (target_node_id, confidence, resolved_by) for EVERY unresolved ref.

use std::path::{Path, PathBuf};

use codegraph_core::types::{FileRecord, Language};
use codegraph_extract::extract_file;
use codegraph_resolve::context::StoreResolutionContext;
use codegraph_resolve::snapshot_context::build_edge_adjacency;
use codegraph_resolve::{ReferenceResolver, SnapshotResolutionContext};
use codegraph_store::Store;

/// Compile-time gate: the snapshot context MUST be `Sync` (it backs the rayon
/// parallel resolve in T4).
fn _assert_sync<T: Sync>() {}
#[allow(dead_code)]
fn _snapshot_is_sync() {
    _assert_sync::<SnapshotResolutionContext>();
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-snapshot-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

fn lang_of(relative: &str) -> Language {
    match Path::new(relative).extension().and_then(|e| e.to_str()) {
        Some("ts") => Language::TypeScript,
        Some("py") => Language::Python,
        Some("java") => Language::Java,
        Some("cpp") => Language::Cpp,
        Some("go") => Language::Go,
        other => panic!("unexpected fixture extension {other:?}"),
    }
}

/// Index `relative_files` from `root` into a fresh store (nodes + contains edges
/// + unresolved refs), exactly as the production index path does.
fn index_fixture(test_name: &str, root: &Path, relative_files: &[&str]) -> (Store, PathBuf) {
    let db_path = temp_db_path(test_name);
    let mut store = Store::open(&db_path).expect("open store");
    for &relative in relative_files {
        let result = extract_file(root, relative).expect("extract file");
        store
            .upsert_file(&FileRecord {
                path: relative.to_string(),
                content_hash: "fixture".to_string(),
                language: lang_of(relative),
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
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
    (store, db_path)
}

/// Run the full differential comparison for a fixture.
fn assert_contexts_equivalent(test_name: &str, root: &Path, relative_files: &[&str]) -> usize {
    let (mut store, db_path) = index_fixture(test_name, root, relative_files);
    let root_str = root.to_string_lossy().to_string();

    // Capture the unresolved refs BEFORE resolution deletes the resolved ones.
    let refs = store.all_unresolved_refs().expect("read unresolved refs");

    // Run the store-backed resolver once so implements/extends edges exist; this
    // is the state in which get_supertypes is meaningful for both contexts.
    let mut resolver = ReferenceResolver::new(root_str.clone());
    {
        let ctx = StoreResolutionContext::new(&store, &root_str);
        resolver.initialize(&ctx);
        resolver.warm_caches(&ctx);
    }
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");

    // Build BOTH contexts over the post-resolution graph.
    let store_ctx = StoreResolutionContext::new(&store, &root_str);
    let edges = build_edge_adjacency(&store).expect("build edge adjacency");
    let mut snapshot_ctx =
        SnapshotResolutionContext::from_store(&store, &root_str).expect("build snapshot");
    snapshot_ctx.set_edge_adjacency(edges);

    // A second resolver warmed identically, used only to call resolve_one over
    // each context (resolve_one is read-only w.r.t. the resolver's deferred lists
    // beyond pushes we ignore here).
    let mut probe = ReferenceResolver::new(root_str.clone());
    probe.initialize(&store_ctx);
    probe.warm_caches(&store_ctx);

    let mut compared = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for unresolved in &refs {
        let view = codegraph_resolve::RefView {
            from_node_id: unresolved.from_node_id.clone(),
            reference_name: unresolved.reference_name.clone(),
            reference_kind: unresolved.reference_kind,
            line: unresolved.line,
            column: unresolved.col,
            file_path: unresolved.file_path.clone(),
            language: unresolved.language,
            is_function_ref: unresolved.is_function_ref,
        };
        let via_store = probe.resolve_one(&view, &store_ctx);
        let via_snapshot = probe.resolve_one(&view, &snapshot_ctx);
        compared += 1;
        if !resolved_eq(&via_store, &via_snapshot) {
            mismatches.push(format!(
                "ref {}@{}:{} ({:?}) store={via_store:?} snapshot={via_snapshot:?}",
                view.reference_name, view.file_path, view.line, view.reference_kind
            ));
        }
    }

    let _ = std::fs::remove_file(&db_path);

    assert!(
        mismatches.is_empty(),
        "{} mismatches out of {compared} refs:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    eprintln!("[{test_name}] {compared} refs resolved through both contexts, 0 mismatches");
    compared
}

fn resolved_eq(
    a: &Option<codegraph_resolve::ResolvedRef>,
    b: &Option<codegraph_resolve::ResolvedRef>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.target_node_id == y.target_node_id
                && x.resolved_by == y.resolved_by
                && (x.confidence - y.confidence).abs() < f64::EPSILON
        }
        _ => false,
    }
}

fn fresh_fixture_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-snap-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture src");
    dir
}

#[test]
fn snapshot_matches_store_over_mini_fixture() {
    // The committed mini fixture exercises import resolution, instance-method
    // name matching, and qualified-name resolution across TS + Python.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/codegraph-bench/fixtures/mini");
    let compared = assert_contexts_equivalent(
        "mini",
        &root,
        &["src/math.ts", "src/app.ts", "tools/greeter.py"],
    );
    assert!(compared > 0, "mini fixture produced no refs to compare");
}

#[test]
fn snapshot_matches_store_over_conformance_supertype_graph() {
    // Java conformance graph: Factory.create().ping() where ping lives on Sub's
    // supertype Base — exercises get_supertypes through the per-chunk edge map.
    let dir = fresh_fixture_dir("conformance");
    std::fs::write(
        dir.join("src/Pingable.java"),
        "interface Pingable { void ping(); }\n",
    )
    .expect("write Pingable.java");
    std::fs::write(
        dir.join("src/Base.java"),
        "class Base implements Pingable { public void ping() {} }\n",
    )
    .expect("write Base.java");
    std::fs::write(dir.join("src/Sub.java"), "class Sub extends Base {}\n")
        .expect("write Sub.java");
    std::fs::write(
        dir.join("src/Factory.java"),
        "class Factory {\n    static Sub create() { return new Sub(); }\n    void run() { Factory.create().ping(); }\n}\n",
    )
    .expect("write Factory.java");

    let compared = assert_contexts_equivalent(
        "conformance",
        &dir,
        &[
            "src/Pingable.java",
            "src/Base.java",
            "src/Sub.java",
            "src/Factory.java",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(compared > 0, "conformance fixture produced no refs");
}

#[test]
fn snapshot_matches_store_over_reexport_barrel_graph() {
    // TS re-export barrel: consumer imports through an index barrel that
    // re-exports from the implementation module — exercises get_re_exports +
    // import mappings.
    let dir = fresh_fixture_dir("reexport");
    std::fs::write(
        dir.join("src/widget.ts"),
        "export function build(): number { return 1; }\nexport class Panel { render(): void {} }\n",
    )
    .expect("write widget.ts");
    std::fs::write(
        dir.join("src/index.ts"),
        "export { build, Panel } from './widget';\nexport * from './widget';\n",
    )
    .expect("write index.ts");
    std::fs::write(
        dir.join("src/app.ts"),
        "import { build, Panel } from './index';\n\nexport function run(): void {\n  build();\n  const p = new Panel();\n  p.render();\n}\n",
    )
    .expect("write app.ts");

    let compared = assert_contexts_equivalent(
        "reexport",
        &dir,
        &["src/widget.ts", "src/index.ts", "src/app.ts"],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(compared > 0, "reexport fixture produced no refs");
}
