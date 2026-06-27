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
        .extract(abs_path, content, "")
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

// ---------------------------------------------------------------------------
// PR2 idFields tests (A3): opt-in bare/compound ID capture as
// `godot:id:<kind>:<value>` sentinel references.
// ---------------------------------------------------------------------------

/// Collect every `godot:id:*` sentinel reference_name, in emission order.
fn id_sentinels(result: &FrameworkResolverExtractionResult) -> Vec<String> {
    result
        .references
        .iter()
        .filter(|r| r.reference_name.starts_with("godot:id:"))
        .map(|r| r.reference_name.clone())
        .collect()
}

#[test]
fn idfields_bare_integer_emits_single_sentinel() {
    // Given a config declaring `buff_id` as an idField of kind `buff`,
    let root = unique_dir("id-bare");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "idFields": { "buff_id": { "kind": "buff" } } } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 7005
";
    let tres = write_tres(&root, "data/strength.tres", content);

    // When extracting a `.tres` with a bare integer `buff_id = 7005`,
    let result = extract(&tres, content);

    // Then exactly one `godot:id:buff:7005` sentinel is emitted, anchored on the
    // resource marker with EdgeKind::References.
    assert_eq!(id_sentinels(&result), vec!["godot:id:buff:7005"]);
    let sentinel = result
        .references
        .iter()
        .find(|r| r.reference_name == "godot:id:buff:7005")
        .expect("the buff sentinel");
    assert_eq!(sentinel.reference_kind, EdgeKind::References);
    let marker = result.nodes.first().expect("a resource marker node");
    assert_eq!(sentinel.from_node_id, marker.id);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn idfields_compound_split_selects_exactly_configured_segments() {
    // Given a config splitting `skill_effect` on `:` and selecting segments [2, 4],
    let root = unique_dir("id-compound");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "idFields": { "skill_effect": { "kind": "skill", "separator": ":", "idSegments": [2, 4] } } } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
skill_effect = \"a:b:9015:c:7005:1000\"
";
    let tres = write_tres(&root, "data/mage.tres", content);

    // When extracting the compound value,
    let result = extract(&tres, content);

    // Then EXACTLY the 0-based segments 2 and 4 (9015, 7005) become sentinels —
    // not the whole string, not segment 5 (1000).
    assert_eq!(
        id_sentinels(&result),
        vec!["godot:id:skill:9015", "godot:id:skill:7005"],
        "only segments [2, 4] are captured, in idSegments order"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn idfields_without_config_emits_zero_id_sentinels() {
    // Given a project with NO codegraph.json at all (the golden-neutrality lock),
    let root = unique_dir("id-no-config");
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 7005
skill_effect = \"a:b:9015:c:7005:1000\"
";
    let tres = write_tres(&root, "data/strength.tres", content);

    // When extracting a `.tres` full of ID-shaped lines,
    let result = extract(&tres, content);

    // Then ZERO id sentinels are emitted — the config is the only trigger, so a
    // non-configured project behaves byte-identically to pre-PR2.
    assert!(
        id_sentinels(&result).is_empty(),
        "without idFields config there must be NO id sentinel, got {:?}",
        result.references
    );
    // No ext_resource either → the off path emits nothing at all.
    assert!(
        result.references.is_empty(),
        "no config + no ext_resource → zero refs, got {:?}",
        result.references
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn idfields_configured_field_absent_from_file_emits_no_sentinel() {
    // Given a config declaring `buff_id` but a `.tres` that never uses it,
    let root = unique_dir("id-absent");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "idFields": { "buff_id": { "kind": "buff" } } } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
duration = 5.0
";
    let tres = write_tres(&root, "data/strength.tres", content);

    // When extracting a `.tres` with no matching key,
    let result = extract(&tres, content);

    // Then no sentinel fires — the spec map is matched by exact key.
    assert!(
        id_sentinels(&result).is_empty(),
        "a configured field absent from the file emits no sentinel, got {:?}",
        result.references
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn idfields_out_of_range_segment_is_silently_skipped() {
    // Given a config selecting an in-range segment 1 and an out-of-range 9,
    let root = unique_dir("id-oob");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "idFields": { "pair": { "kind": "x", "separator": ":", "idSegments": [1, 9] } } } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
pair = \"100:200\"
";
    let tres = write_tres(&root, "data/oob.tres", content);

    // When extracting a value with only two segments,
    let result = extract(&tres, content);

    // Then the in-range segment yields a sentinel and the out-of-range index is
    // silently skipped (no panic, no error, no empty sentinel).
    assert_eq!(
        id_sentinels(&result),
        vec!["godot:id:x:200"],
        "in-range segment 1 captured; out-of-range 9 dropped"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn idfields_emission_is_deterministic_across_runs() {
    // Given a config with two idFields and a `.tres` using both,
    let root = unique_dir("id-determinism");
    write_config(
        &root,
        r#"{ "godot": { "dsl": { "idFields": { "buff_id": { "kind": "buff" }, "skill_effect": { "kind": "skill", "separator": ":", "idSegments": [0, 2] } } } } }"#,
    );
    let content = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 7005
skill_effect = \"9015:c:7005\"
";
    let tres = write_tres(&root, "data/spell.tres", content);

    // When extracting twice,
    let a = extract(&tres, content);
    let b = extract(&tres, content);

    // Then the sentinel set, order, and edge sources are byte-stable, and follow
    // SOURCE-LINE order (buff_id line before skill_effect line), not config order.
    assert_eq!(
        id_sentinels(&a),
        id_sentinels(&b),
        "deterministic across runs"
    );
    assert_eq!(
        id_sentinels(&a),
        vec![
            "godot:id:buff:7005",
            "godot:id:skill:9015",
            "godot:id:skill:7005"
        ],
        "sentinels follow source-line order then idSegments order"
    );

    let _ = std::fs::remove_dir_all(&root);
}
