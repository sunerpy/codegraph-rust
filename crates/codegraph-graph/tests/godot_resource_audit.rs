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
/// A `project.godot` with an `[autoload]` singleton pointing at a script; kept
/// separate from the autoload-free `PROJECT_GODOT` that sibling tests reuse.
const PROJECT_GODOT_AUTOLOAD: &str =
    "config_version=5\n\n[autoload]\nBuffManager=\"*res://buff_manager.gd\"\n";
/// A `project.godot` whose only resource ref is a `run/main_scene` path.
const PROJECT_GODOT_MAIN_SCENE: &str =
    "config_version=5\n\n[application]\nrun/main_scene=\"res://main.tscn\"\n";

/// A `.gd` script exposing one handler method, and a `.tscn` that binds the
/// script to its root node and wires a `pressed` signal to that handler via a
/// `[connection method="..."]`. The connection emits a bare-name unresolved ref
/// (`reference_name=<method>`, `language=GodotScene`) — the input the dangling
/// narrowing must NOT report as a missing path.
fn handler_script(method: &str) -> String {
    format!("extends Button\n\nfunc {method}():\n\tpass\n")
}

fn scene_with_connection(script_rel: &str, method: &str) -> String {
    format!(
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://{script_rel}\" id=\"1\"]\n\n[node name=\"Root\" type=\"Button\"]\nscript = ExtResource(\"1\")\n\n[connection signal=\"pressed\" from=\".\" to=\".\" method=\"{method}\"]\n"
    )
}

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
fn impact_carries_the_graph_edge_kind() {
    // Given target.tres referenced by data.tres via an ExtResource ref,
    let dir = unique_dir("impact-edgekind");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    // When impact is computed for target.tres,
    let store = run_pipeline(
        "impact-edgekind",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("target.tres")
        .expect("impact");
    // Then the data.tres site carries the graph EDGE kind that links it.
    let data_ref = impact
        .affected
        .iter()
        .find(|a| a.from_file == "data.tres")
        .expect("data.tres in impact");
    assert!(
        data_ref.edge_kind == "references" || data_ref.edge_kind == "instantiates",
        "edge_kind must surface the graph edge kind, got: {:?}",
        data_ref.edge_kind
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn impact_dedup_keeps_distinct_edge_kinds_at_a_shared_site() {
    // Given a resource referenced both as an unresolved ref AND a resolved edge
    // (impact merges both sources before sort+dedup),
    let dir = unique_dir("impact-dedup");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    // When impact is computed,
    let store = run_pipeline(
        "impact-dedup",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("target.tres")
        .expect("impact");
    // Then for any shared (from_file, line) the rows differ only by edge_kind,
    // and dedup keeps one row per distinct (from_file, line, edge_kind) tuple —
    // pinning the post-edge_kind dedup behaviour as intentional + deterministic.
    let data_rows: Vec<_> = impact
        .affected
        .iter()
        .filter(|a| a.from_file == "data.tres")
        .collect();
    let mut seen = std::collections::HashSet::new();
    for row in &data_rows {
        assert!(
            seen.insert((row.from_file.clone(), row.line, row.edge_kind.clone())),
            "dedup must leave no duplicate (from_file,line,edge_kind), got: {data_rows:?}"
        );
    }
    assert!(
        !data_rows.is_empty(),
        "expected at least one data.tres row, got: {impact:?}"
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

#[test]
fn existing_signal_handler_method_is_not_dangling() {
    // Given a scene wiring a `pressed` signal to `_on_Existing`, whose method
    // exists in the attached player.gd,
    let dir = unique_dir("dangling-signal-existing");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "player.gd", &handler_script("_on_Existing"));
    write(
        &dir,
        "main.tscn",
        &scene_with_connection("player.gd", "_on_Existing"),
    );
    // When dangling detection runs,
    let store = run_pipeline(
        "dangling-signal-existing",
        &dir,
        &["project.godot", "player.gd", "main.tscn"],
    );
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then the bare handler name is NOT reported as a missing path.
    assert!(
        !dangling.iter().any(|d| d.target_path == "_on_Existing"),
        "existing signal handler must not be dangling, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nonexistent_signal_handler_method_is_still_not_reported_by_dangling() {
    // Given a scene wiring a signal to `_on_Missing`, a method that does NOT
    // exist in the attached player.gd (only `_on_Existing` does),
    let dir = unique_dir("dangling-signal-missing");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "player.gd", &handler_script("_on_Existing"));
    write(
        &dir,
        "main.tscn",
        &scene_with_connection("player.gd", "_on_Missing"),
    );
    // When dangling detection runs,
    let store = run_pipeline(
        "dangling-signal-missing",
        &dir,
        &["project.godot", "player.gd", "main.tscn"],
    );
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then it is STILL not reported — dangling reports missing PATHS only, not
    // signal-method resolution (the documented scope boundary).
    assert!(
        !dangling.iter().any(|d| d.target_path == "_on_Missing"),
        "dangling reports missing paths only, never bare signal methods, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn genuine_missing_ext_resource_path_is_still_dangling() {
    // Given a scene whose attached script path res://Data/Missing.tres does not
    // exist on disk (alongside an existing-handler connection),
    let dir = unique_dir("dangling-missing-path");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(
        &dir,
        "main.tscn",
        &scene_with_connection("Data/Missing.tres", "_on_Existing"),
    );
    // When dangling detection runs,
    let store = run_pipeline(
        "dangling-missing-path",
        &dir,
        &["project.godot", "main.tscn"],
    );
    let dangling = GraphTraverser::new(&store)
        .find_dangling_references(&dir)
        .expect("dangling");
    // Then the genuine missing PATH is still reported, while the bare handler
    // name is not.
    assert!(
        dangling
            .iter()
            .any(|d| d.target_path == "Data/Missing.tres"),
        "missing ExtResource path must still be dangling, got: {dangling:?}"
    );
    assert!(
        !dangling.iter().any(|d| d.target_path == "_on_Existing"),
        "bare handler name must not be dangling, got: {dangling:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn orphan_detection_unchanged_by_the_dangling_narrowing() {
    // Given target.tres referenced only via a path ref from data.tres — the
    // exact orphan-accounting input that keys on path-shaped resource refs (all
    // of which contain `/`), so the dangling-only narrowing must not touch it,
    let dir = unique_dir("orphan-unchanged");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "target.tres", PLAIN_RESOURCE);
    write(&dir, "data.tres", &referencing_resource("target.tres"));
    // When orphan detection runs,
    let store = run_pipeline(
        "orphan-unchanged",
        &dir,
        &["project.godot", "target.tres", "data.tres"],
    );
    let orphans = GraphTraverser::new(&store)
        .find_orphan_resources()
        .expect("orphans");
    // Then target.tres stays non-orphan (referenced) — output unchanged.
    assert!(
        !orphans.iter().any(|o| o.file_path == "target.tres"),
        "orphan output must be unchanged by the dangling narrowing, got: {orphans:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autoload_ref_carries_the_autoload_edge_subkind() {
    // Given a project.godot [autoload] singleton pointing at buff_manager.gd,
    let dir = unique_dir("autoload-subkind");
    write(&dir, "project.godot", PROJECT_GODOT_AUTOLOAD);
    write(
        &dir,
        "buff_manager.gd",
        "extends Node\n\nfunc _ready():\n\tpass\n",
    );
    // When impact is computed for the autoloaded script,
    let store = run_pipeline(
        "autoload-subkind",
        &dir,
        &["project.godot", "buff_manager.gd"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("buff_manager.gd")
        .expect("impact");
    // Then the project.godot row surfaces edge_subkind == "autoload".
    assert!(
        impact
            .affected
            .iter()
            .any(|a| a.edge_subkind == Some("autoload".to_string())),
        "autoload edge must carry edge_subkind=autoload, got: {impact:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn main_scene_ref_is_not_tagged_autoload() {
    // Given a project.godot main_scene ref (NOT an autoload) to main.tscn,
    let dir = unique_dir("main-scene-not-autoload");
    write(&dir, "project.godot", PROJECT_GODOT_MAIN_SCENE);
    write(
        &dir,
        "main.tscn",
        "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node\"]\n",
    );
    // When impact is computed for the main scene,
    let store = run_pipeline(
        "main-scene-not-autoload",
        &dir,
        &["project.godot", "main.tscn"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("main.tscn")
        .expect("impact");
    // Then no row is mistagged as autoload (main-scene shares reference()).
    assert!(
        !impact
            .affected
            .iter()
            .any(|a| a.edge_subkind == Some("autoload".to_string())),
        "main-scene ref must NOT be tagged autoload, got: {impact:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn impact_surfaces_gdscript_preload_via_imports_edge() {
    // Given a .gd that `preload`s another .gd (a resolved `imports` edge tagged
    // gdscript_load_path by the walker),
    let dir = unique_dir("impact-preload-imports");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "scripts/x.gd", "extends Node\n\nfunc go():\n\tpass\n");
    write(
        &dir,
        "loader.gd",
        "extends Node\n\nconst X = preload(\"res://scripts/x.gd\")\n",
    );
    // When impact is computed for the preloaded script,
    let store = run_pipeline(
        "impact-preload-imports",
        &dir,
        &["project.godot", "scripts/x.gd", "loader.gd"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("scripts/x.gd")
        .expect("impact");
    // Then the preloading site surfaces as an `imports` edge carrying the
    // gdscript_load_path subkind (previously excluded from resource_impact).
    let row = impact
        .affected
        .iter()
        .find(|a| a.from_file == "loader.gd")
        .unwrap_or_else(|| panic!("expected loader.gd in impact, got: {impact:?}"));
    assert_eq!(
        row.edge_kind, "imports",
        "preload must surface as an imports edge, got: {row:?}"
    );
    assert_eq!(
        row.edge_subkind,
        Some("gdscript_load_path".to_string()),
        "imports edge must carry the gdscript_load_path subkind, got: {row:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn impact_surfaces_gdscript_extends_path_via_extends_edge() {
    // Given a .gd that `extends "res://base.gd"` (a resolved `extends` edge
    // tagged gdscript_load_path by the walker),
    let dir = unique_dir("impact-extends-path");
    write(&dir, "project.godot", PROJECT_GODOT);
    write(&dir, "base.gd", "extends Node\n\nfunc base_fn():\n\tpass\n");
    write(
        &dir,
        "child.gd",
        "extends \"res://base.gd\"\n\nfunc go():\n\tpass\n",
    );
    // When impact is computed for the base script,
    let store = run_pipeline(
        "impact-extends-path",
        &dir,
        &["project.godot", "base.gd", "child.gd"],
    );
    let impact = GraphTraverser::new(&store)
        .resource_impact("base.gd")
        .expect("impact");
    // Then the extending site surfaces as an `extends` edge carrying the
    // gdscript_load_path subkind (previously excluded from resource_impact).
    let row = impact
        .affected
        .iter()
        .find(|a| a.from_file == "child.gd")
        .unwrap_or_else(|| panic!("expected child.gd in impact, got: {impact:?}"));
    assert_eq!(
        row.edge_kind, "extends",
        "extends-path must surface as an extends edge, got: {row:?}"
    );
    assert_eq!(
        row.edge_subkind,
        Some("gdscript_load_path".to_string()),
        "extends edge must carry the gdscript_load_path subkind, got: {row:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
