//! L5 Godot static-analysis tests: OPTIONAL, config-gated DSL resource-field
//! reference edges (T9 of godot-static-analysis).
//!
//! The DSL hook is OFF by default. It fires ONLY when a `.codegraph/codegraph.json`
//! up the directory tree from the `.tres` declares
//! `godot.dsl.resourceFields = [...]`. Each listed `[resource]` property name `F`
//! then makes its `F = <value>` line emit a [`EdgeKind::References`] edge from the
//! resource marker to the value (a string literal → the literal text; an
//! `ExtResource("id")` handle → the resolved repo-relative path, via the same
//! id-table T5 uses for `script`/property bindings).
//!
//! These tests build a temp project dir (config + `.tres`) and drive the public
//! [`FrameworkResolver::extract`] entry point with the ABSOLUTE `.tres` path, so
//! the config reader can walk up to the config exactly as the pipeline does.

use std::path::{Path, PathBuf};

use codegraph_core::types::EdgeKind;
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::godot::GodotResolver;
use codegraph_resolve::types::FrameworkResolverExtractionResult;

/// A fresh, uniquely-named temp project directory.
fn unique_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-godot-l5-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    dir
}

/// Write `.codegraph/codegraph.json` with the given JSON contents under `root`.
fn write_config(root: &Path, json: &str) {
    let cfg_dir = root.join(".codegraph");
    std::fs::create_dir_all(&cfg_dir).expect("mkdir .codegraph");
    std::fs::write(cfg_dir.join("codegraph.json"), json).expect("write codegraph.json");
}

/// Write `rel` under `root` and return its ABSOLUTE path as a `/`-joined string
/// (the config reader walks up from this path).
fn write_tres(root: &Path, rel: &str, content: &str) -> String {
    let abs = root.join(rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).expect("mkdir tres parent");
    }
    std::fs::write(&abs, content).expect("write tres");
    abs.to_string_lossy().into_owned()
}

/// Extract a `.tres` at `abs_path` (panics if the resolver returned `None`).
fn extract(abs_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    GodotResolver
        .extract(abs_path, content)
        .expect(".tres must produce Some(result)")
}

const SKILL_TRES: &str = "\
[gd_resource type=\"Resource\" format=3]

[resource]
skill_effect = \"Fireball\"
duration = 5.0
";

#[test]
fn with_dsl_config_string_field_emits_reference_to_literal_value() {
    // Given a project whose codegraph.json lists `skill_effect` as a DSL
    // resource field,
    let root = unique_dir("with-config");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "resourceFields": ["skill_effect"] } } }"#,
    );
    let tres = write_tres(&root, "data/strength.tres", SKILL_TRES);

    // When extracting the `.tres` that has `skill_effect = "Fireball"`,
    let result = extract(&tres, SKILL_TRES);

    // Then a reference edge to the literal value `Fireball` is emitted.
    let dsl_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "Fireball")
        .expect("a DSL reference to 'Fireball'");
    assert_eq!(
        dsl_ref.reference_kind,
        EdgeKind::References,
        "DSL edge must reuse EdgeKind::References"
    );

    // And it anchors on a resource marker node (the same lazily-created marker
    // T5 uses), so the edge is attributable.
    let marker = result
        .nodes
        .first()
        .expect("a resource marker node must exist for the DSL edge");
    assert_eq!(
        dsl_ref.from_node_id, marker.id,
        "DSL ref must originate from the resource marker node"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_dsl_config_same_tres_emits_no_dsl_edge() {
    // Given a project with NO codegraph.json at all (the common case),
    let root = unique_dir("no-config");
    let tres = write_tres(&root, "data/strength.tres", SKILL_TRES);

    // When extracting the SAME `.tres`,
    let result = extract(&tres, SKILL_TRES);

    // Then ZERO DSL edges are emitted — `skill_effect = "Fireball"` produces no
    // reference, because the config is the only trigger.
    assert!(
        result
            .references
            .iter()
            .all(|r| r.reference_name != "Fireball"),
        "without config there must be NO DSL edge, got {:?}",
        result.references
    );
    // This `.tres` has no ext_resource either, so T5 emits nothing — total zero.
    assert!(
        result.references.is_empty(),
        "no config + no ext_resource → zero refs, got {:?}",
        result.references
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dsl_config_present_but_other_fields_listed_emits_no_edge() {
    // Given a config that lists a DIFFERENT field (not `skill_effect`),
    let root = unique_dir("other-field");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "resourceFields": ["effect_on"] } } }"#,
    );
    let tres = write_tres(&root, "data/strength.tres", SKILL_TRES);

    // When extracting a `.tres` whose only DSL-shaped line is `skill_effect`,
    let result = extract(&tres, SKILL_TRES);

    // Then no DSL edge fires — the field list is honored exactly.
    assert!(
        result.references.is_empty(),
        "a non-matching field list must emit no DSL edge, got {:?}",
        result.references
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dsl_field_with_ext_resource_value_resolves_to_path() {
    // Given a config listing `skill_effect` and a `.tres` whose `skill_effect`
    // value is an ExtResource handle with a matching declaration,
    let root = unique_dir("ext-resource");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "resourceFields": ["skill_effect"] } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[ext_resource type=\"Resource\" path=\"res://skills/fireball.tres\" id=\"1\"]

[resource]
skill_effect = ExtResource(\"1\")
";
    let tres = write_tres(&root, "data/mage.tres", content);

    // When extracting,
    let result = extract(&tres, content);

    // Then the DSL edge resolves the ExtResource id to the repo-relative path
    // (res:// stripped), exactly like T5's script/property bindings.
    let dsl_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "skills/fireball.tres")
        .expect("DSL ref to the resolved ExtResource path");
    assert_eq!(dsl_ref.reference_kind, EdgeKind::References);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn malformed_config_is_ignored_no_panic_no_dsl_edge() {
    // Given a malformed codegraph.json (truncated JSON),
    let root = unique_dir("malformed");
    write_config(&root, r#"{ "godot": { "dsl": { "resourceFields": ["#);
    let tres = write_tres(&root, "data/strength.tres", SKILL_TRES);

    // When extracting (must not panic),
    let result = extract(&tres, SKILL_TRES);

    // Then the malformed config is ignored — no DSL edge, as if absent.
    assert!(
        result.references.is_empty(),
        "malformed config must yield zero DSL edges, got {:?}",
        result.references
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dsl_parsing_is_deterministic_across_runs() {
    // Given a config listing two DSL fields and a `.tres` using both,
    let root = unique_dir("determinism");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "resourceFields": ["skill_effect", "effect_on"] } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
skill_effect = \"Fireball\"
effect_on = \"Enemy\"
";
    let tres = write_tres(&root, "data/spell.tres", content);

    // When extracting twice,
    let a = extract(&tres, content);
    let b = extract(&tres, content);

    // Then the parser-controlled fields (ids/names/kinds/targets/order) match.
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
    assert_eq!(nodes_a, nodes_b, "node ids/names/order deterministic");

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
        "DSL ref source/target/kind/order deterministic"
    );
    // Both DSL targets are present, in source order.
    assert_eq!(
        refs_a.iter().map(|(_, name, _)| *name).collect::<Vec<_>>(),
        vec!["Fireball", "Enemy"],
        "both DSL fields emit edges in source order"
    );

    let _ = std::fs::remove_dir_all(&root);
}
