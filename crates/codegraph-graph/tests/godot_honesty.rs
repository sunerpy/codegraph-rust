//! T8 (L6) Godot honesty-signal tests for the graph-level detection.
//!
//! Grounds the "no static caller != dead" rule: a symbol reached only via a
//! Godot dynamic/structural link (a `.tscn` `[connection]` handler ref, an
//! autoload script binding) must report as dynamically reachable, and the
//! `godot:dynamic:` sentinel refs must surface as a distinct category. The
//! detection is gated strictly on the PRESENCE of such Godot links — a
//! non-Godot symbol with no callers reports nothing.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_graph::graph::GraphTraverser;
use codegraph_store::Store;

fn temp_db_path(test_name: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "codegraph-godot-honesty-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    file_path: &str,
    language: Language,
    signature: Option<&str>,
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: file_path.to_string(),
        language,
        start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: signature.map(str::to_string),
        visibility: None,
        is_exported: false,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: 1,
    }
}

fn unresolved(
    from_node_id: &str,
    reference_name: &str,
    kind: EdgeKind,
    file_path: &str,
    language: Language,
) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: from_node_id.to_string(),
        reference_name: reference_name.to_string(),
        reference_kind: kind,
        line: 1,
        col: 0,
        candidates: None,
        file_path: file_path.to_string(),
        language,
        is_function_ref: false,
    }
}

#[test]
fn connection_handler_func_is_dynamically_reachable_not_dead() {
    // Given a GDScript `func _on_hit` whose only inbound link is a `.tscn`
    // `[connection method="_on_hit"]` ref (an UNRESOLVED ref from a GodotScene
    // file, name-matching the function),
    // When the dynamic-reachability of `_on_hit` is queried,
    // Then it reports reachable-via-Godot (signal/scene connection).
    let mut store = Store::open(&temp_db_path("conn-handler")).expect("open store");
    let on_hit = node(
        "function:on_hit",
        NodeKind::Function,
        "_on_hit",
        "player.gd",
        Language::Gdscript,
        Some("()"),
    );
    let scene = node(
        "constant:player_scene_node",
        NodeKind::Constant,
        "Player",
        "Main.tscn",
        Language::GodotScene,
        None,
    );
    store.upsert_nodes(&[on_hit.clone(), scene]).unwrap();
    store
        .insert_unresolved_refs(&[unresolved(
            "constant:player_scene_node",
            "_on_hit",
            EdgeKind::References,
            "Main.tscn",
            Language::GodotScene,
        )])
        .unwrap();

    let traverser = GraphTraverser::new(&store);
    let reach = traverser
        .godot_dynamic_reachability(&on_hit)
        .expect("reachability query");
    assert!(
        reach.is_dynamically_reachable(),
        "a .tscn connection handler must be flagged dynamically reachable, got {reach:#?}"
    );
}

#[test]
fn autoload_bound_func_is_dynamically_reachable_not_dead() {
    // Given `func apply` in `buff_manager.gd`, which is the script bound to the
    // `BuffManager` autoload singleton (a project.godot Constant whose signature
    // records `autoload -> buff_manager.gd`),
    // When the dynamic-reachability of `apply` is queried,
    // Then it reports reachable-via-Godot (autoload).
    let mut store = Store::open(&temp_db_path("autoload-bound")).expect("open store");
    let apply = node(
        "function:apply",
        NodeKind::Function,
        "apply",
        "buff_manager.gd",
        Language::Gdscript,
        Some("()"),
    );
    let singleton = node(
        "constant:buff_manager",
        NodeKind::Constant,
        "BuffManager",
        "project.godot",
        Language::GodotProject,
        Some("autoload -> buff_manager.gd"),
    );
    store.upsert_nodes(&[apply.clone(), singleton]).unwrap();

    let traverser = GraphTraverser::new(&store);
    let reach = traverser
        .godot_dynamic_reachability(&apply)
        .expect("reachability query");
    assert!(
        reach.is_dynamically_reachable(),
        "an autoload-bound function must be flagged dynamically reachable, got {reach:#?}"
    );
}

#[test]
fn dynamic_unresolved_sentinel_is_surfaced_for_its_origin() {
    // Given a `func dyn` containing `get_node(some_var)` — emitted as a
    // `godot:dynamic:get_node` sentinel unresolved ref from that function,
    // When the dynamic-reachability of `dyn` is queried,
    // Then the sentinel surfaces as a dynamic/unresolved entry.
    let mut store = Store::open(&temp_db_path("dyn-sentinel")).expect("open store");
    let dyn_fn = node(
        "function:dyn",
        NodeKind::Function,
        "dyn",
        "player.gd",
        Language::Gdscript,
        Some("()"),
    );
    store.upsert_nodes(std::slice::from_ref(&dyn_fn)).unwrap();
    store
        .insert_unresolved_refs(&[unresolved(
            "function:dyn",
            "godot:dynamic:get_node",
            EdgeKind::References,
            "player.gd",
            Language::Gdscript,
        )])
        .unwrap();

    let traverser = GraphTraverser::new(&store);
    let reach = traverser
        .godot_dynamic_reachability(&dyn_fn)
        .expect("reachability query");
    assert_eq!(
        reach.dynamic_unresolved,
        vec!["godot:dynamic:get_node".to_string()],
        "the get_node(var) sentinel must surface as a dynamic/unresolved entry, got {reach:#?}"
    );
}

#[test]
fn non_godot_symbol_has_no_dynamic_signal() {
    // Given an ordinary TypeScript function with NO callers and NO Godot links
    // anywhere in the store,
    // When its dynamic-reachability is queried,
    // Then there is NO dynamic signal at all (the annotation must never appear
    // for a non-Godot project).
    let mut store = Store::open(&temp_db_path("non-godot")).expect("open store");
    let helper = node(
        "function:helper",
        NodeKind::Function,
        "helper",
        "src/util.ts",
        Language::TypeScript,
        Some("(): void"),
    );
    store.upsert_nodes(std::slice::from_ref(&helper)).unwrap();
    // A same-named unresolved ref, but from a NON-Godot (TS) file, must NOT count.
    store
        .insert_unresolved_refs(&[unresolved(
            "function:other",
            "helper",
            EdgeKind::Calls,
            "src/other.ts",
            Language::TypeScript,
        )])
        .unwrap();

    let traverser = GraphTraverser::new(&store);
    let reach = traverser
        .godot_dynamic_reachability(&helper)
        .expect("reachability query");
    assert!(
        !reach.is_dynamically_reachable(),
        "a non-Godot symbol must never get a dynamic-reachability signal, got {reach:#?}"
    );
    assert!(
        reach.dynamic_unresolved.is_empty(),
        "a non-Godot symbol must surface no dynamic/unresolved refs, got {reach:#?}"
    );
    assert!(
        !reach.has_any_signal(),
        "a non-Godot symbol must produce no Godot honesty signal whatsoever"
    );
}
