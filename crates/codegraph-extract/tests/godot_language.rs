use codegraph_core::types::{Language, NodeKind};
use codegraph_extract::{detect_language, extract_source};

#[test]
fn godot_detects_scene_extension() {
    assert_eq!(detect_language("scenes/foo.tscn"), Language::GodotScene);
    assert_eq!(detect_language("foo.tscn"), Language::GodotScene);
}

#[test]
fn godot_detects_resource_extension() {
    assert_eq!(detect_language("data/foo.tres"), Language::GodotResource);
    assert_eq!(detect_language("foo.tres"), Language::GodotResource);
}

#[test]
fn godot_detects_project_file_by_basename() {
    // project.godot has no extension; detect_language must special-case the
    // file_name() == "project.godot" BEFORE the extension lookup.
    assert_eq!(
        detect_language("/some/dir/project.godot"),
        Language::GodotProject
    );
    // bare basename, no directory.
    assert_eq!(detect_language("project.godot"), Language::GodotProject);
}

#[test]
fn godot_does_not_regress_gdscript() {
    // .gd must still detect as Gdscript (no regression from the new variants).
    assert_eq!(detect_language("scripts/player.gd"), Language::Gdscript);
}

#[test]
fn godot_scene_is_file_level_only() {
    // A .tscn source yields a result with NO symbol nodes and NO edges — it is
    // file-level-only for now (file node is attached by the walker, not by
    // extract_source). Mirrors Yaml/Twig/Properties behavior.
    let source = "[gd_scene load_steps=2 format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n";
    let result = extract_source("scenes/foo.tscn", source, Some(Language::GodotScene));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(
        result.nodes.is_empty(),
        "godot scene must emit zero symbol nodes; nodes={:#?}",
        result.nodes
    );
    assert!(result.edges.is_empty(), "edges={:#?}", result.edges);
    assert!(
        result.unresolved_references.is_empty(),
        "refs={:#?}",
        result.unresolved_references
    );
    assert!(
        !result
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Function | NodeKind::Class)),
        "no functions/classes for a scene file"
    );
}

#[test]
fn godot_resource_is_file_level_only() {
    let source = "[gd_resource type=\"Resource\" format=3]\n\n[resource]\n";
    let result = extract_source("data/foo.tres", source, Some(Language::GodotResource));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(result.nodes.is_empty(), "nodes={:#?}", result.nodes);
    assert!(result.edges.is_empty(), "edges={:#?}", result.edges);
}

#[test]
fn godot_project_is_file_level_only() {
    let source = "config_version=5\n\n[application]\nconfig/name=\"Demo\"\n";
    let result = extract_source("project.godot", source, Some(Language::GodotProject));
    assert!(result.errors.is_empty(), "errors={:#?}", result.errors);
    assert!(result.nodes.is_empty(), "nodes={:#?}", result.nodes);
    assert!(result.edges.is_empty(), "edges={:#?}", result.edges);
}
