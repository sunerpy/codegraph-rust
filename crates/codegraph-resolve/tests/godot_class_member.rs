//! GDScript `ClassName.member()` static-call resolution tests (T2).
//!
//! Mirrors the autoload-access harness (`godot_post_extract.rs`): index files
//! (base extraction + `extract_and_persist_frameworks`), run
//! `resolve_and_persist` (the generic pass, where the `GodotResolver::resolve`
//! step produces the class-member edge), then inspect the resolved edges.
//!
//! A GDScript `class_name Foo` global's members are file-level `Function`
//! nodes in the SAME file as the `Class` node. A call `Foo.bar()` is extracted
//! as an unresolved `Calls` ref `reference_name = "Foo.bar"`. The resolver step
//! maps the `Class` node NAME `Foo` → its `file_path`, then resolves the ref to
//! the `Function` named `bar` in that file — emitted at confidence 0.9 as a
//! framework edge.
//!
//! What is verified:
//! - Positive: `class_name Foo` + `func bar()` in one file, `Foo.bar()` in
//!   another → a resolved Calls edge to `bar` (resolvedBy = framework).
//! - Negative: a lowercase `foo.bar()` instance call → NO framework edge.
//! - Negative: a `Recv.member` where `Recv` is NOT a GDScript class node name →
//!   NO framework edge.

use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn unique_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-godot-classmember-{slug}-{}-{}",
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
        "codegraph-godot-classmember-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

/// Index `relative_files` under `root` into a fresh store and run the entire
/// resolver pipeline (detect → framework extract → resolve → post_extract).
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

/// The `Function` node id named `name` (a GDScript file-level function).
fn function_id(store: &Store, name: &str) -> String {
    store
        .nodes_by_name(name)
        .expect("nodes by name")
        .into_iter()
        .find(|n| n.kind == NodeKind::Function && n.language == Language::Gdscript)
        .unwrap_or_else(|| panic!("expected a GDScript Function named {name}"))
        .id
}

/// Write a project.godot marker so the Godot resolver's `detect()` gate passes,
/// a `class_name Foo` global defining `static func bar()`, and a caller that
/// invokes `Foo.bar()`.
fn write_class_member_fixture(root: &Path) {
    std::fs::write(root.join("project.godot"), "config_version=5\n").expect("write project.godot");
    std::fs::write(
        root.join("foo.gd"),
        "class_name Foo\nextends Node\n\nstatic func bar():\n\treturn 1\n",
    )
    .expect("write foo.gd");
    std::fs::write(
        root.join("caller.gd"),
        "extends Node\n\nfunc run():\n\tFoo.bar()\n",
    )
    .expect("write caller.gd");
}

#[test]
fn class_name_static_call_resolves_to_function_in_class_file() {
    // Given `class_name Foo` + `static func bar()` in foo.gd and `Foo.bar()` in caller.gd,
    // When the full pipeline runs,
    // Then there is a resolved framework edge whose target is the `bar` Function.
    let dir = unique_dir("classmember-pos");
    write_class_member_fixture(&dir);

    let (store, edges) = run_pipeline(
        "classmember-pos",
        &dir,
        &["project.godot", "foo.gd", "caller.gd"],
    );
    let bar = function_id(&store, "bar");

    let to_bar: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.target == bar && e.kind == EdgeKind::Calls)
        .collect();
    assert!(
        !to_bar.is_empty(),
        "expected a resolved Calls edge to `bar`, got: {edges:#?}"
    );
    assert_eq!(
        to_bar[0]
            .metadata
            .as_ref()
            .and_then(|m| m["resolvedBy"].as_str()),
        Some("framework"),
        "class-member edge must be produced by the framework resolver, got: {:#?}",
        to_bar[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lowercase_instance_call_yields_no_framework_edge() {
    // Given `class_name Foo` + a lowercase instance call `foo.bar()`,
    // When the pipeline runs,
    // Then no framework edge is produced for the lowercase instance receiver.
    let dir = unique_dir("classmember-neg-lower");
    std::fs::write(dir.join("project.godot"), "config_version=5\n").expect("write project.godot");
    std::fs::write(
        dir.join("foo.gd"),
        "class_name Foo\nextends Node\n\nstatic func bar():\n\treturn 1\n",
    )
    .expect("write foo.gd");
    // A lowercase receiver `foo` is an instance variable, NOT the class global.
    std::fs::write(
        dir.join("caller.gd"),
        "extends Node\n\nvar foo = Foo.new()\n\nfunc run():\n\tfoo.bar()\n",
    )
    .expect("write caller.gd");

    let (store, edges) = run_pipeline(
        "classmember-neg-lower",
        &dir,
        &["project.godot", "foo.gd", "caller.gd"],
    );
    let bar = function_id(&store, "bar");

    let framework_to_bar: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            e.target == bar
                && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("framework")
        })
        .collect();
    assert!(
        framework_to_bar.is_empty(),
        "a lowercase `foo.bar()` instance call must not be framework-resolved, got: {framework_to_bar:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_class_receiver_yields_no_framework_edge() {
    // Given a `Recv.member()` call where `Recv` is NOT a GDScript class node,
    // When the pipeline runs,
    // Then no framework edge is fabricated for the unknown receiver.
    let dir = unique_dir("classmember-neg-nonclass");
    std::fs::write(dir.join("project.godot"), "config_version=5\n").expect("write project.godot");
    // foo.gd defines `func bar()` at file level but has NO `class_name` — so
    // there is no GDScript Class node named `Foo`.
    std::fs::write(
        dir.join("foo.gd"),
        "extends Node\n\nstatic func bar():\n\treturn 1\n",
    )
    .expect("write foo.gd");
    std::fs::write(
        dir.join("caller.gd"),
        "extends Node\n\nfunc run():\n\tNotAClass.bar()\n",
    )
    .expect("write caller.gd");

    let (store, edges) = run_pipeline(
        "classmember-neg-nonclass",
        &dir,
        &["project.godot", "foo.gd", "caller.gd"],
    );
    let bar = function_id(&store, "bar");

    let framework_to_bar: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            e.target == bar
                && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("framework")
        })
        .collect();
    assert!(
        framework_to_bar.is_empty(),
        "a `NotAClass.bar()` call (no class node) must not produce a framework edge, got: {framework_to_bar:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
