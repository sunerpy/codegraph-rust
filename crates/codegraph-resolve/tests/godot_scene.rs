//! L2 Godot static-analysis tests: `.tscn` scene parsing via
//! [`GodotResolver::extract`] (T4 of godot-static-analysis).
//!
//! These exercise the public [`FrameworkResolver::extract`] dispatch — the
//! resolver-pipeline entry point — so the assertions pin the observable
//! extraction shape, not internals. T4 adds the `.tscn` branch; `.tres` (T5)
//! still returns `None`.

use codegraph_core::types::{EdgeKind, NodeKind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::godot::GodotResolver;
use codegraph_resolve::types::FrameworkResolverExtractionResult;

/// Run `extract()` and unwrap the result (panics if the resolver returned
/// `None`, which for a `.tscn` is itself a failure).
fn extract(path: &str, content: &str) -> FrameworkResolverExtractionResult {
    GodotResolver
        .extract(path, content)
        .expect(".tscn must produce Some(result)")
}

#[test]
fn node_with_script_emits_node_and_script_reference() {
    // Given a scene declaring a Script ext_resource and a node that binds it,
    let content = "\
[gd_scene load_steps=2 format=3]

[ext_resource type=\"Script\" path=\"res://player.gd\" id=\"1_abc\"]

[node name=\"Player\" type=\"CharacterBody2D\"]
script = ExtResource(\"1_abc\")
";
    // When extracting,
    let result = extract("scenes/Player.tscn", content);

    // Then there is a scene node named "Player" with the reused Constant kind.
    let player = result
        .nodes
        .iter()
        .find(|n| n.name == "Player")
        .expect("Player node");
    assert_eq!(player.kind, NodeKind::Constant, "scene-node kind");

    // And a scene→script reference resolves the ExtResource id to the
    // repo-relative script path (res:// stripped), originating from the node.
    let script_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "player.gd")
        .expect("script ref to player.gd");
    assert_eq!(
        script_ref.from_node_id, player.id,
        "script ref must originate from the Player node"
    );
}

#[test]
fn connection_emits_reference_to_handler_method() {
    // Given a [connection] wiring a signal to a handler method,
    let content = "\
[gd_scene format=3]

[node name=\"Timer\" type=\"Timer\"]

[connection signal=\"timeout\" from=\"Timer\" to=\".\" method=\"_on_timeout\"]
";
    // When extracting,
    let result = extract("scenes/Main.tscn", content);

    // Then a reference to the handler method NAME is emitted (T7 resolves the
    // method to its actual symbol; T4 only names it).
    let handler = result
        .references
        .iter()
        .find(|r| r.reference_name == "_on_timeout")
        .expect("ref to handler _on_timeout");
    assert_eq!(handler.reference_kind, EdgeKind::References);
}

#[test]
fn node_groups_emit_group_membership_references() {
    // Given a node with a `groups = [...]` membership list,
    let content = "\
[gd_scene format=3]

[node name=\"Goblin\" type=\"Node2D\"]
groups = [\"enemies\", \"hostiles\"]
";
    // When extracting,
    let result = extract("scenes/Goblin.tscn", content);

    // Then a group-membership reference per group name is emitted.
    let group_refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        group_refs.contains(&"enemies"),
        "expected `enemies` group ref, got {group_refs:?}"
    );
    assert!(
        group_refs.contains(&"hostiles"),
        "expected `hostiles` group ref, got {group_refs:?}"
    );
}

#[test]
fn instanced_subscene_emits_instantiates_reference() {
    // Given a node instancing a PackedScene ext_resource,
    let content = "\
[gd_scene load_steps=2 format=3]

[ext_resource type=\"PackedScene\" path=\"res://enemy.tscn\" id=\"2\"]

[node name=\"EnemySpawn\" type=\"Node2D\" instance=ExtResource(\"2\")]
";
    // When extracting,
    let result = extract("scenes/Level.tscn", content);

    // Then an Instantiates edge to the resolved .tscn path is emitted.
    let instance_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "enemy.tscn")
        .expect("instance ref to enemy.tscn");
    assert_eq!(
        instance_ref.reference_kind,
        EdgeKind::Instantiates,
        "instanced subscene must use Instantiates"
    );
}

#[test]
fn malformed_section_and_line_skipped_without_panic() {
    // Given a scene with a malformed section header and a junk line between two
    // valid nodes,
    let content = "\
[gd_scene format=3]

[node name=\"A\" type=\"Node\"]
this_line_has_no_equals

[this is not a valid header
[node name=\"B\" type=\"Node\"]
";
    // When extracting (must not panic),
    let result = extract("scenes/Junk.tscn", content);

    // Then both valid nodes still parse.
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"), "got {names:?}");
    assert!(names.contains(&"B"), "got {names:?}");
}

#[test]
fn parsing_is_deterministic_across_runs() {
    // Given identical scene content,
    let content = "\
[gd_scene load_steps=2 format=3]

[ext_resource type=\"Script\" path=\"res://player.gd\" id=\"1_abc\"]

[node name=\"Player\" type=\"CharacterBody2D\"]
script = ExtResource(\"1_abc\")
groups = [\"players\"]

[connection signal=\"hit\" from=\"Player\" to=\".\" method=\"_on_hit\"]
";
    // When extracting twice,
    let a = extract("scenes/Player.tscn", content);
    let b = extract("scenes/Player.tscn", content);

    // `updated_at` is a wall-clock value the shared `framework_node` helper
    // stamps, so it is EXCLUDED here — two extract() calls can straddle a
    // millisecond boundary under load. Assert only parser-controlled fields.
    let nodes_a: Vec<(&str, &str, NodeKind)> = a
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str(), n.kind))
        .collect();
    let nodes_b: Vec<(&str, &str, NodeKind)> = b
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str(), n.kind))
        .collect();
    assert_eq!(
        nodes_a, nodes_b,
        "node ids/names/kinds/order must be deterministic"
    );

    let refs_a: Vec<(&str, &str, EdgeKind)> = a
        .references
        .iter()
        .map(|r| {
            (
                r.from_node_id.as_str(),
                r.reference_name.as_str(),
                r.reference_kind,
            )
        })
        .collect();
    let refs_b: Vec<(&str, &str, EdgeKind)> = b
        .references
        .iter()
        .map(|r| {
            (
                r.from_node_id.as_str(),
                r.reference_name.as_str(),
                r.reference_kind,
            )
        })
        .collect();
    assert_eq!(
        refs_a, refs_b,
        "reference source/target/kind/order must be deterministic"
    );
}

#[test]
fn extract_routes_only_tscn_not_gd_or_tres() {
    // A .tscn dispatches to T4.
    assert!(GodotResolver
        .extract("scenes/Main.tscn", "[gd_scene format=3]\n")
        .is_some());
    // A nested path whose extension is .tscn still dispatches.
    assert!(GodotResolver
        .extract("a/b/c/Deep.tscn", "[gd_scene format=3]\n")
        .is_some());

    // A .gd file now routes to T6's GDScript dynamic parser (Some).
    assert!(GodotResolver
        .extract("player.gd", "extends Node\n")
        .is_some());
    // A .tres routes to T5's resource parser (Some, via that parser, not this).
    assert!(GodotResolver
        .extract("data/item.tres", "[gd_resource format=3]\n")
        .is_some());
    // project.godot still routes to T3 (not this parser) — it returns Some, but
    // via the project parser, so it is NOT None.
    assert!(GodotResolver
        .extract("project.godot", "[autoload]\nX=\"res://x.gd\"\n")
        .is_some());
}

#[test]
fn script_reference_with_unknown_ext_resource_id_is_skipped() {
    // Given a node binding a script id that has no matching ext_resource,
    let content = "\
[gd_scene format=3]

[node name=\"Orphan\" type=\"Node\"]
script = ExtResource(\"999_missing\")
";
    // When extracting (must not panic),
    let result = extract("scenes/Orphan.tscn", content);

    // Then the node still exists but no script ref is emitted (nothing to
    // resolve the id to).
    assert!(
        result.nodes.iter().any(|n| n.name == "Orphan"),
        "Orphan node must still parse"
    );
    assert!(
        result.references.is_empty(),
        "no ref for an unresolvable ExtResource id, got {:?}",
        result.references
    );
}
