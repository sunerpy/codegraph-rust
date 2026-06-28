//! L4 Godot static-analysis tests: `.tres` resource parsing via
//! [`GodotResolver::extract`] (T5 of godot-static-analysis).
//!
//! These exercise the public [`FrameworkResolver::extract`] dispatch — the
//! resolver-pipeline entry point — so the assertions pin the observable
//! extraction shape, not internals. T5 adds the `.tres` branch; a `.tres` is a
//! single flat resource (no scene-tree), so its edges anchor on ONE resource
//! marker node rather than per-scene-node as in T4.

use codegraph_core::types::{EdgeKind, NodeKind, ReferenceSubkind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::godot::GodotResolver;
use codegraph_resolve::types::FrameworkResolverExtractionResult;

/// Run `extract()` and unwrap the result (panics if the resolver returned
/// `None`, which for a `.tres` is itself a failure).
fn extract(path: &str, content: &str) -> FrameworkResolverExtractionResult {
    GodotResolver
        .extract(path, content, "")
        .expect(".tres must produce Some(result)")
}

#[test]
fn resource_script_emits_reference_to_script_path() {
    // Given a `.tres` declaring a Script ext_resource and binding it under
    // [resource] via `script = ExtResource("id")`,
    let content = "\
[gd_resource type=\"Resource\" script_class=\"Buff\" load_steps=2 format=3]

[ext_resource type=\"Script\" path=\"res://buff.gd\" id=\"1\"]

[resource]
script = ExtResource(\"1\")
duration = 5.0
";
    // When extracting,
    let result = extract("data/strength_buff.tres", content);

    // Then a resource→script reference resolves the ExtResource id to the
    // repo-relative script path (res:// stripped).
    let script_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "buff.gd")
        .expect("script ref to buff.gd");
    assert_eq!(
        script_ref.reference_kind,
        EdgeKind::References,
        "script binding must use References"
    );

    // And the edge originates from a resource marker node (NodeKind::Constant,
    // consistent with T3/T4) — i.e. there is a node and the ref's source is it.
    let marker = result
        .nodes
        .first()
        .expect("a resource marker node must exist");
    assert_eq!(marker.kind, NodeKind::Constant, "resource-marker kind");
    assert_eq!(
        script_ref.from_node_id, marker.id,
        "script ref must originate from the resource marker node"
    );
}

#[test]
fn resource_property_ext_resource_emits_resource_reference() {
    // Given a `.tres` whose [resource] property points at another resource via
    // `effect = ExtResource("2")` (a Buff pointing at an effect .tres),
    let content = "\
[gd_resource type=\"Resource\" format=3]

[ext_resource type=\"Resource\" path=\"res://effects/fire.tres\" id=\"2\"]

[resource]
effect = ExtResource(\"2\")
";
    // When extracting,
    let result = extract("data/burn_buff.tres", content);

    // Then a resource→resource reference to the resolved .tres path is emitted.
    let effect_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "effects/fire.tres")
        .expect("resource ref to effects/fire.tres");
    assert_eq!(
        effect_ref.reference_kind,
        EdgeKind::References,
        "resource→resource binding must use References"
    );
}

#[test]
fn resource_with_no_ext_resource_emits_zero_reference_edges() {
    // Given a `.tres` with no ext_resource declarations (a self-contained
    // resource — only inline scalar properties),
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
amount = 10
label = \"Health Potion\"
";
    // When extracting (must not panic),
    let result = extract("data/potion.tres", content);

    // Then NO reference edges are emitted (no spurious edges); the file node
    // from T1 carries the file on its own.
    assert!(
        result.references.is_empty(),
        "a resource with no ext_resource must emit zero refs, got {:?}",
        result.references
    );
    // And no resource marker node is emitted when there is nothing to anchor.
    assert!(
        result.nodes.is_empty(),
        "no marker node without any reference, got {:?}",
        result.nodes
    );
}

#[test]
fn malformed_section_and_line_skipped_without_panic() {
    // Given a `.tres` with a malformed section header and a junk line,
    let content = "\
[gd_resource type=\"Resource\" format=3]

[ext_resource type=\"Script\" path=\"res://ok.gd\" id=\"1\"]

[this is not a valid header
[resource]
this_line_has_no_equals
script = ExtResource(\"1\")
";
    // When extracting (must not panic),
    let result = extract("data/junk.tres", content);

    // Then the valid script ref still resolves despite the surrounding junk.
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "ok.gd"),
        "valid script ref must survive the junk, got {:?}",
        result.references
    );
}

#[test]
fn unknown_ext_resource_id_is_skipped() {
    // Given a [resource] binding an id that has no matching ext_resource,
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
script = ExtResource(\"999_missing\")
";
    // When extracting (must not panic),
    let result = extract("data/orphan.tres", content);

    // Then no ref is emitted (nothing to resolve the id to), and consequently
    // no marker node either.
    assert!(
        result.references.is_empty(),
        "no ref for an unresolvable ExtResource id, got {:?}",
        result.references
    );
}

#[test]
fn parsing_is_deterministic_across_runs() {
    // Given identical resource content with both a script and a resource ref,
    let content = "\
[gd_resource type=\"Resource\" format=3]

[ext_resource type=\"Script\" path=\"res://buff.gd\" id=\"1\"]
[ext_resource type=\"Resource\" path=\"res://effects/fire.tres\" id=\"2\"]

[resource]
script = ExtResource(\"1\")
effect = ExtResource(\"2\")
";
    // When extracting twice,
    let a = extract("data/buff.tres", content);
    let b = extract("data/buff.tres", content);

    // Then every parser-controlled field matches across runs. (The `updated_at`
    // field is the same wall-clock `Date.now()` value T3/T4 nodes carry, so it
    // is intentionally excluded from the determinism check — the parser controls
    // ids, names, kinds, and reference targets/kinds/order, not the clock.)
    let nodes_a: Vec<(&str, &str)> = a
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();
    let nodes_b: Vec<(&str, &str)> = b
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();
    assert_eq!(
        nodes_a, nodes_b,
        "node ids/names/order must be deterministic"
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
fn extract_routes_tres_to_t5_and_others_correctly() {
    // A .tres dispatches to T5 (Some).
    assert!(GodotResolver
        .extract("data/item.tres", "[gd_resource format=3]\n", "")
        .is_some());
    // A nested path whose extension is .tres still dispatches.
    assert!(GodotResolver
        .extract("a/b/c/Deep.tres", "[gd_resource format=3]\n", "")
        .is_some());

    // A .gd file now routes to T6's GDScript dynamic parser (Some).
    assert!(GodotResolver
        .extract("player.gd", "extends Node\n", "")
        .is_some());
    // A .tscn routes to T4 (Some, via the scene parser, not this one).
    assert!(GodotResolver
        .extract("scenes/Main.tscn", "[gd_scene format=3]\n", "")
        .is_some());
    // project.godot routes to T3 (Some, via the project parser).
    assert!(GodotResolver
        .extract("project.godot", "[autoload]\nX=\"res://x.gd\"\n", "")
        .is_some());
}

#[test]
fn resource_ext_resource_refs_carry_ext_resource_subkind() {
    // Given a `.tres` binding a script and another resource via ExtResource,
    let content = "\
[gd_resource type=\"Resource\" load_steps=3 format=3]

[ext_resource type=\"Script\" path=\"res://buff.gd\" id=\"1\"]
[ext_resource type=\"Resource\" path=\"res://effect.tres\" id=\"2\"]

[resource]
script = ExtResource(\"1\")
effect = ExtResource(\"2\")
";
    // When extracting,
    let result = extract("data/buff.tres", content);

    // Then every resource ExtResource ref carries the ExtResource subkind.
    assert!(
        !result.references.is_empty(),
        "expected ExtResource refs, got none"
    );
    for reference in &result.references {
        assert_eq!(
            reference.reference_subkind,
            Some(ReferenceSubkind::ExtResource),
            "ref {} must carry ExtResource subkind",
            reference.reference_name
        );
    }
}
