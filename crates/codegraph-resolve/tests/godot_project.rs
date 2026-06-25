//! L1 Godot static-analysis tests: `project.godot` parsing via
//! [`GodotResolver::extract`] (T3 of godot-static-analysis).
//!
//! These exercise the public [`FrameworkResolver::extract`] dispatch — the
//! resolver-pipeline entry point T4/T5 extend with `.tscn`/`.tres` branches —
//! so the assertions pin the observable extraction shape, not internals.

use codegraph_core::types::{EdgeKind, NodeKind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::godot::GodotResolver;
use codegraph_resolve::types::FrameworkResolverExtractionResult;

/// Run `extract()` and unwrap the result (panics if the resolver returned
/// `None`, which for `project.godot` is itself a failure).
fn extract(path: &str, content: &str) -> FrameworkResolverExtractionResult {
    GodotResolver
        .extract(path, content)
        .expect("project.godot must produce Some(result)")
}

#[test]
fn autoload_section_emits_one_node_and_ref_per_singleton() {
    // Given a project.godot with two autoloads (one enabled `*`, one plain),
    let content = "\
[autoload]

GameState=\"*res://globals/game_state.gd\"
Music=\"res://audio/music.gd\"
";
    // When extracting,
    let result = extract("project.godot", content);

    // Then there is exactly one node per autoload name.
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"GameState"), "got node names {names:?}");
    assert!(names.contains(&"Music"), "got node names {names:?}");
    let autoload_nodes: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.name == "GameState" || n.name == "Music")
        .collect();
    assert_eq!(autoload_nodes.len(), 2, "exactly two autoload nodes");

    // And each autoload node carries an honest, reused NodeKind (Constant).
    for n in &autoload_nodes {
        assert_eq!(n.kind, NodeKind::Constant, "autoload kind for {}", n.name);
    }

    // And each emits a References edge to its target script with res:// stripped
    // to a repo-relative path (the leading `*` is dropped too).
    let ref_targets: Vec<&str> = result
        .references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::References)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        ref_targets.contains(&"globals/game_state.gd"),
        "expected globals/game_state.gd ref, got {ref_targets:?}"
    );
    assert!(
        ref_targets.contains(&"audio/music.gd"),
        "expected audio/music.gd ref, got {ref_targets:?}"
    );
}

#[test]
fn autoload_ref_originates_from_its_singleton_node() {
    // Given one autoload,
    let content = "[autoload]\nGameState=\"*res://globals/game_state.gd\"\n";
    // When extracting,
    let result = extract("project.godot", content);

    // Then the singleton→script ref's from_node_id is exactly that node's id.
    let node = result
        .nodes
        .iter()
        .find(|n| n.name == "GameState")
        .expect("GameState node");
    let edge = result
        .references
        .iter()
        .find(|r| r.reference_name == "globals/game_state.gd")
        .expect("ref to script");
    assert_eq!(
        edge.from_node_id, node.id,
        "ref must originate from singleton"
    );
}

#[test]
fn main_scene_emits_a_reference_to_the_scene_path() {
    // Given an [application] run/main_scene,
    let content = "\
[application]

config/name=\"Demo\"
run/main_scene=\"res://main.tscn\"
";
    // When extracting,
    let result = extract("project.godot", content);

    // Then a reference to the scene path (res:// stripped) is present.
    let has_main_scene = result
        .references
        .iter()
        .any(|r| r.reference_name == "main.tscn");
    assert!(
        has_main_scene,
        "expected main.tscn ref, got {:?}",
        result
            .references
            .iter()
            .map(|r| &r.reference_name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn malformed_autoload_line_is_skipped_without_panic() {
    // Given an [autoload] section with one malformed line (no `=`) between two
    // valid singletons,
    let content = "\
[autoload]
GameState=\"*res://globals/game_state.gd\"
this_line_has_no_equals_sign
Music=\"res://audio/music.gd\"
";
    // When extracting (must not panic),
    let result = extract("project.godot", content);

    // Then the two valid singletons still parse.
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"GameState"), "got {names:?}");
    assert!(names.contains(&"Music"), "got {names:?}");
    // And the malformed token produced no node.
    assert!(
        !names.contains(&"this_line_has_no_equals_sign"),
        "malformed line must be skipped, got {names:?}"
    );
}

#[test]
fn empty_or_sectionless_file_yields_empty_result() {
    // Given a project.godot with no sections at all,
    let result = extract("project.godot", "\n; a comment\nconfig_version=5\n");
    // Then no nodes and no references are emitted (top-level keys are not
    // autoloads).
    assert!(result.nodes.is_empty(), "no nodes: {:?}", result.nodes);
    assert!(
        result.references.is_empty(),
        "no refs: {:?}",
        result.references
    );

    // And a fully empty file is also empty.
    let empty = extract("project.godot", "");
    assert!(empty.nodes.is_empty());
    assert!(empty.references.is_empty());
}

#[test]
fn parsing_is_deterministic_across_runs() {
    // Given identical content,
    let content = "\
[autoload]
GameState=\"*res://globals/game_state.gd\"
Music=\"res://audio/music.gd\"

[application]
run/main_scene=\"res://main.tscn\"
";
    // When extracting twice,
    let a = extract("project.godot", content);
    let b = extract("project.godot", content);

    // Then node ids and the full node/ref ordering are byte-identical.
    let ids_a: Vec<&str> = a.nodes.iter().map(|n| n.id.as_str()).collect();
    let ids_b: Vec<&str> = b.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids_a, ids_b, "node ids must be deterministic");
    assert_eq!(a, b, "the whole extraction result must be identical");
}

#[test]
fn input_actions_each_emit_a_node() {
    // Given an [input] section with two action keys,
    let content = "\
[input]

move_left={
\"deadzone\": 0.5
}
jump={
\"deadzone\": 0.5
}
";
    // When extracting,
    let result = extract("project.godot", content);

    // Then a node exists per action name.
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"move_left"), "got {names:?}");
    assert!(names.contains(&"jump"), "got {names:?}");
}

#[test]
fn extract_returns_none_for_non_project_godot_file() {
    // A .gd file is not this layer's job — extract() returns None.
    assert!(GodotResolver.extract("foo.gd", "extends Node\n").is_none());
    // A .tres routes to T5's resource parser (Some, not this project parser).
    assert!(GodotResolver
        .extract("data/item.tres", "[gd_resource]\n")
        .is_some());
    // A nested path whose basename IS project.godot still dispatches.
    assert!(GodotResolver
        .extract("sub/dir/project.godot", "[autoload]\nX=\"res://x.gd\"\n")
        .is_some());
}
