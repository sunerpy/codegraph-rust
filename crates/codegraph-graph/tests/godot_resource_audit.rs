//! Graph-level tests for the read-only Godot resource audit (B1/B2/B3).
//!
//! Each test indexes an inline Godot fixture through the full resolver pipeline,
//! then drives `find_orphan_resources` / `find_dangling_references` /
//! `resource_impact` and asserts the report. The B0 probe established that godot
//! `.tres`/`.tscn` files carry no `file:` node and their inbound references stay
//! in `unresolved_refs`, so the audit is keyed on PATHS, not incoming edges.

use std::path::{Path, PathBuf};

use codegraph_core::types::{FileRecord, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_graph::graph::GraphTraverser;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn unique_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-audit-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    dir
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-audit-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

fn write(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(full, content).expect("write fixture file");
}

fn run_pipeline(test_name: &str, root: &Path, relative_files: &[&str]) -> Store {
    let mut store = Store::open(&temp_db_path(test_name)).expect("open store");
    for &relative in relative_files {
        let language = detect_language(relative);
        let result = extract_file(root, relative).expect("extract file");
        store
            .upsert_file(&FileRecord {
                path: relative.to_string(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store.insert_edges(&result.edges).expect("insert edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }
    let mut resolver = ReferenceResolver::new(root.to_string_lossy().to_string());
    {
        let context =
            codegraph_resolve::StoreResolutionContext::new(&store, root.to_string_lossy());
        resolver.initialize(&context);
    }
    let relative: Vec<String> = relative_files.iter().map(|f| (*f).to_string()).collect();
    resolver
        .extract_and_persist_frameworks(&mut store, &relative)
        .expect("framework extract");
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");
    resolver.run_post_extract(&mut store).expect("post extract");
    store
}

const PLAIN_RESOURCE: &str = "[gd_resource type=\"Resource\" format=3]\n\n[resource]\n";
const PROJECT_GODOT: &str = "config_version=5\n\n[application]\nconfig/name=\"Audit Fixture\"\n";

fn referencing_resource(target: &str) -> String {
    format!(
        "[gd_resource type=\"Resource\" format=3]\n\n[ext_resource type=\"Resource\" path=\"res://{target}\" id=\"1\"]\n\n[resource]\nlinked = ExtResource(\"1\")\n"
    )
}

#[test]
fn resource_with_no_incoming_reference_is_orphan() {
    // Given a lone .tres that nothing references,
    let dir = unique_dir("orphan-pos");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "lonely.tres", PLAIN_RESOURCE);
    // When orphan detection runs,
    let store = run_pipeline("orphan-pos", &dir, &["project.godot", "lonely.tres"]);
    let orphans = GraphTraverser::new(&store)
        .find_orphan_resources()
        .expect("orphans");
    // Then it is reported as an orphan.
    assert!(orphans.iter().any(|o| o.file_path == "lonely.tres"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resource_with_an_incoming_reference_is_not_orphan() {
    // Given target.tres referenced by data.tres,
    let dir = unique_dir("orphan-neg");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    // When orphan detection runs,
    let store = run_pipeline(
        "orphan-neg",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );
    let orphans = GraphTraverser::new(&store)
        .find_orphan_resources()
        .expect("orphans");
    // Then target.tres is NOT orphan (data.tres references it).
    assert!(
        !orphans.iter().any(|o| o.file_path == "target.tres"),
        "target.tres must not be orphan, got: {orphans:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_shaped_ref_to_missing_on_disk_path_is_dangling() {
    // Given data.tres referencing a .tres that does not exist on disk,
    let dir = unique_dir("dangling-pos");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(
        &dir,
        "data.tres",
        &referencing_resource("missing/ghost.tres"),
    );
    // When dangling detection runs against the project root,
    let store = run_pipeline("dangling-pos", &dir, &["project.godot", "data.tres"]);
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then the missing target is reported.
    assert!(
        dangling
            .iter()
            .any(|d| d.target_path == "missing/ghost.tres"),
        "expected missing/ghost.tres dangling, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn godot_dynamic_ref_is_not_dangling() {
    // Given a .gd whose dynamic call produces a godot:dynamic: unresolved ref,
    let dir = unique_dir("dangling-dynamic");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(
        &dir,
        "player.gd",
        "extends Node\n\nfunc act(name):\n\tget_node(name).call(\"go\")\n",
    );
    // When dangling detection runs,
    let store = run_pipeline("dangling-dynamic", &dir, &["project.godot", "player.gd"]);
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then no godot:dynamic: ref is reported as dangling.
    assert!(
        !dangling
            .iter()
            .any(|d| d.target_path.starts_with("godot:dynamic:")),
        "godot:dynamic: refs must never be dangling, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_addons_ref_is_not_dangling_prefix_beats_disk_check() {
    // Given data.tres referencing a MISSING addons/ path (so only the prefix
    // exclusion — not disk existence — can spare it),
    let dir = unique_dir("dangling-addons");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(
        &dir,
        "data.tres",
        &referencing_resource("addons/plugin/missing.tres"),
    );
    // When dangling detection runs,
    let store = run_pipeline("dangling-addons", &dir, &["project.godot", "data.tres"]);
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then the addons/ ref is excluded despite being absent on disk.
    assert!(
        !dangling
            .iter()
            .any(|d| d.target_path.starts_with("addons/")),
        "addons/ refs must be excluded by prefix before the disk check, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn impact_lists_the_referencing_files() {
    // Given target.tres referenced by data.tres,
    let dir = unique_dir("impact");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    // When impact is computed for target.tres,
    let store = run_pipeline(
        "impact",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("target.tres")
        .expect("impact");
    // Then data.tres is listed as a referencing site.
    assert_eq!(impact.changed, "target.tres");
    assert!(
        impact.affected.iter().any(|a| a.from_file == "data.tres"),
        "expected data.tres in impact, got: {impact:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn orphan_accounting_is_keyed_on_resource_path_not_a_file_node() {
    // Given a referenced .tres — and confirmation that godot resources carry no
    // file: node (the B0 finding the orphan model relies on),
    let dir = unique_dir("orphan-keying");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    let store = run_pipeline(
        "orphan-keying",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );

    // When we inspect the graph,
    let file_nodes = store.nodes_by_kind(NodeKind::File).expect("file nodes");
    // Then there is NO file: node for either .tres (accounting must be by path),
    assert!(
        !file_nodes
            .iter()
            .any(|n| n.file_path == "target.tres" || n.file_path == "data.tres"),
        ".tres files must have no file: node, got: {file_nodes:?}"
    );
    // And orphan accounting (keyed on path) still marks target.tres referenced.
    let orphans = GraphTraverser::new(&store)
        .find_orphan_resources()
        .expect("orphans");
    assert!(
        !orphans.iter().any(|o| o.file_path == "target.tres"),
        "path-keyed accounting must mark target.tres as referenced, got: {orphans:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
