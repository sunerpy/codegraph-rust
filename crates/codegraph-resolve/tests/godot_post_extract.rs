//! L7 cross-file finalization tests for the Godot resolver.
//!
//! Drives the FULL resolver pipeline against a populated `Store` — index files
//! (base extraction + `extract_and_persist_frameworks`), run
//! `resolve_and_persist` (the generic pass, where autoload-access edges are
//! produced by `GodotResolver::resolve`), then `run_post_extract` (the
//! nodes-only finalization that stamps the confirmed singleton→script binding).
//!
//! What is verified:
//! - Autoload access (`BuffManager.apply()`) resolves to the autoload singleton
//!   node (a `project.godot` `Constant`), produced by the generic pass.
//! - A call to a NON-autoload (`NotAnAutoload.foo()`) yields NO autoload edge —
//!   the roster gate rejects unknown receivers, so no edge is fabricated.
//! - `post_extract` stamps the confirmed script path onto the singleton and is
//!   deterministic / idempotent.

use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn unique_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-godot-l7-{slug}-{}-{}",
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
        "codegraph-godot-l7-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

/// Index `relative_files` under `root` into a fresh store and run the entire
/// resolver pipeline (detect → framework extract → resolve → post_extract).
/// Returns `(store, resolved_edges)` so callers can inspect both edges and the
/// post_extract node mutations.
fn run_pipeline(test_name: &str, root: &Path, relative_files: &[&str]) -> (Store, Vec<Edge>) {
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
        store
            .insert_edges(&result.edges)
            .expect("insert contains edges");
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
    resolver
        .run_post_extract(&mut store)
        .expect("run post extract");

    let edges = all_resolved_edges(&store);
    (store, edges)
}

fn all_resolved_edges(store: &Store) -> Vec<Edge> {
    let mut resolved = Vec::new();
    for kind in NodeKind::ALL {
        for node in store.nodes_by_kind(kind).expect("nodes by kind") {
            for edge in store
                .edges_by_source_kind(&node.id, None)
                .expect("edges by source")
            {
                if edge.kind != EdgeKind::Contains {
                    resolved.push(edge);
                }
            }
        }
    }
    resolved
}

/// Write the standard 3-file Godot fixture: `project.godot` registering
/// `BuffManager` as an autoload bound to `buff_manager.gd`, the autoload script
/// defining `func apply()`, and a player script that calls both a real autoload
/// (`BuffManager.apply()`) and a non-autoload (`NotAnAutoload.foo()`).
fn write_autoload_fixture(root: &Path) {
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[autoload]\n\nBuffManager=\"*res://buff_manager.gd\"\n",
    )
    .expect("write project.godot");
    std::fs::write(
        root.join("buff_manager.gd"),
        "extends Node\n\nfunc apply():\n\treturn 1\n",
    )
    .expect("write buff_manager.gd");
    std::fs::write(
        root.join("player.gd"),
        "extends Node\n\nfunc use_buff():\n\tBuffManager.apply()\n\tNotAnAutoload.foo()\n",
    )
    .expect("write player.gd");
}

/// The autoload singleton node id for `BuffManager` (a `project.godot` Constant).
fn autoload_singleton_id(store: &Store) -> String {
    store
        .nodes_by_name("BuffManager")
        .expect("nodes by name")
        .into_iter()
        .find(|n| n.kind == NodeKind::Constant)
        .expect("BuffManager autoload singleton node")
        .id
}

#[test]
fn autoload_access_resolves_to_singleton_node() {
    // Given a project.godot autoload + a .gd calling BuffManager.apply(),
    // When the full pipeline runs,
    // Then there is a resolved edge whose target is the BuffManager singleton.
    let dir = unique_dir("autoload-pos");
    write_autoload_fixture(&dir);

    let (store, edges) = run_pipeline(
        "autoload-pos",
        &dir,
        &["project.godot", "buff_manager.gd", "player.gd"],
    );
    let singleton = autoload_singleton_id(&store);

    let to_singleton: Vec<&Edge> = edges.iter().filter(|e| e.target == singleton).collect();
    assert!(
        !to_singleton.is_empty(),
        "expected a resolved edge to the BuffManager autoload singleton, got: {edges:#?}"
    );
    let edge = to_singleton[0];
    assert_eq!(
        edge.metadata
            .as_ref()
            .and_then(|m| m["resolvedBy"].as_str()),
        Some("framework"),
        "autoload edge must be produced by the framework resolver, got: {edge:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_autoload_call_yields_no_autoload_edge() {
    // Given the same fixture (only BuffManager is an autoload),
    // When the pipeline runs,
    // Then NotAnAutoload.foo() produces NO framework-resolved edge — the roster
    // gate rejects the unknown receiver and never fabricates an edge.
    let dir = unique_dir("autoload-neg");
    write_autoload_fixture(&dir);

    let (store, edges) = run_pipeline(
        "autoload-neg",
        &dir,
        &["project.godot", "buff_manager.gd", "player.gd"],
    );

    // There is no autoload singleton named NotAnAutoload at all.
    let not_autoload = store
        .nodes_by_name("NotAnAutoload")
        .expect("nodes by name")
        .into_iter()
        .find(|n| {
            n.kind == NodeKind::Constant
                && n.language == codegraph_core::types::Language::GodotProject
        });
    assert!(
        not_autoload.is_none(),
        "NotAnAutoload must not be an autoload singleton node"
    );

    // No framework-resolved edge may carry the NotAnAutoload.foo reference.
    let fabricated: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("framework")
                && e.line == Some(6)
        })
        .collect();
    assert!(
        fabricated.is_empty(),
        "NotAnAutoload.foo() must not produce a framework edge, got: {fabricated:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn post_extract_stamps_confirmed_script_binding_on_singleton() {
    // Given the autoload fixture (buff_manager.gd exists in the store),
    // When the pipeline runs,
    // Then the BuffManager singleton's signature records the confirmed binding.
    let dir = unique_dir("autoload-stamp");
    write_autoload_fixture(&dir);

    let (store, _edges) = run_pipeline(
        "autoload-stamp",
        &dir,
        &["project.godot", "buff_manager.gd", "player.gd"],
    );

    let singleton = store
        .nodes_by_name("BuffManager")
        .expect("nodes by name")
        .into_iter()
        .find(|n| n.kind == NodeKind::Constant)
        .expect("singleton");
    assert_eq!(
        singleton.signature.as_deref(),
        Some("autoload -> buff_manager.gd"),
        "post_extract must stamp the confirmed script path onto the singleton"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn post_extract_does_not_stamp_when_script_missing() {
    // Given a project.godot autoload pointing at a script that is NOT indexed,
    // When the pipeline runs,
    // Then the singleton signature is left untouched (no fabricated binding).
    let dir = unique_dir("autoload-missing");
    std::fs::write(
        dir.join("project.godot"),
        "[autoload]\n\nGhost=\"*res://ghost.gd\"\n",
    )
    .expect("write project.godot");

    let (store, _edges) = run_pipeline("autoload-missing", &dir, &["project.godot"]);

    let singleton = store
        .nodes_by_name("Ghost")
        .expect("nodes by name")
        .into_iter()
        .find(|n| n.kind == NodeKind::Constant)
        .expect("singleton");
    assert!(
        singleton.signature.is_none(),
        "a singleton whose script is absent must not be stamped, got: {:?}",
        singleton.signature
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pipeline_is_deterministic_across_runs() {
    // Given the autoload fixture,
    // When the pipeline runs twice into independent stores,
    // Then the resolved-edge key sets (kind/source/target/resolvedBy) are equal —
    // determinism on parser-controlled fields, excluding wall-clock updated_at.
    let dir = unique_dir("autoload-determinism");
    write_autoload_fixture(&dir);

    let key = |edges: &[Edge]| -> Vec<(String, String, String, Option<String>)> {
        let mut v: Vec<(String, String, String, Option<String>)> = edges
            .iter()
            .map(|e| {
                (
                    e.kind.as_str().to_string(),
                    e.source.clone(),
                    e.target.clone(),
                    e.metadata
                        .as_ref()
                        .and_then(|m| m["resolvedBy"].as_str())
                        .map(str::to_string),
                )
            })
            .collect();
        v.sort();
        v
    };

    let (_s1, e1) = run_pipeline(
        "autoload-determinism-a",
        &dir,
        &["project.godot", "buff_manager.gd", "player.gd"],
    );
    let (_s2, e2) = run_pipeline(
        "autoload-determinism-b",
        &dir,
        &["project.godot", "buff_manager.gd", "player.gd"],
    );
    assert_eq!(
        key(&e1),
        key(&e2),
        "resolved-edge key set must be identical across runs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
