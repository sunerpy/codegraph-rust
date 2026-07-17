//! `project.godot` parser — L1 of Godot static analysis (T3).
//!
//! Parses the INI-like `project.godot` manifest into framework nodes +
//! references, the per-file half of the autoload-singleton graph
//! ([`crate::frameworks::godot::GodotResolver::extract`] dispatches here when a
//! file's basename is `project.godot`). Cross-file resolution of the emitted
//! reference paths to their target script/scene nodes is T7's `post_extract`
//! job; this layer only parses + emits.
//!
//! # Format (the subset L1 reads)
//!
//! `project.godot` is a flat INI: `[section]` headers and `key=value` lines.
//! L1 reads four sections:
//!
//! - `[autoload]` — `Name="[*]res://path/to/x.gd"`. Each line is a global
//!   singleton: `Name` is the global identifier, the value is the bound
//!   script/scene path. A leading `*` inside the quotes means "enabled as a
//!   singleton" (vs. a plain preload); L1 strips it. Emits one node per `Name`
//!   plus a `References` edge `Name → path`.
//! - `[application]` — `run/main_scene="res://main.tscn"`. Emits a `References`
//!   edge from the synthesized main-scene node to the scene path.
//! - `[input]` — `action_name={ ... }`. Emits one node per action name (the
//!   key); the value (a dictionary literal, possibly multi-line) is ignored.
//! - `[editor_plugins]` — `enabled=PackedStringArray("res://addons/x/plugin.cfg",
//!   ...)`. Emits a `References` edge per enabled plugin path.
//!
//! # NodeKind choice
//!
//! Synthesized Godot nodes (autoload singletons, the main-scene marker, input
//! actions, enabled plugins) all use [`NodeKind::Constant`]: an autoload is a
//! globally-accessible, immutable-by-name binding established at engine init and
//! referenced by that name everywhere — semantically a named global constant.
//! This reuses an existing kind (no new `NodeKind` variant, per the golden
//! blast-radius constraint); React's `extract` reuses `Component`/`Function`
//! and NestJS reuses `Route` the same way.
//!
//! # res:// mapping
//!
//! A `res://` path is the Godot project-root URI. L1 maps it to a repo-relative
//! path by stripping a single leading `*` (autoload-enabled marker), the
//! `res://` scheme, and any remaining leading `/` — e.g.
//! `*res://globals/game_state.gd` → `globals/game_state.gd`. No further
//! resolution (cross-file symbol binding is T7).
//!
//! # Tolerance
//!
//! Every line is parsed defensively: a line with no `=` (outside a section
//! header), an unterminated value, or an unknown section is skipped, never
//! panics. An empty or sectionless file yields an empty result.

use std::collections::BTreeMap;

use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, ReferenceSubkind};

use super::framework_node;
use super::godot_common::{map_res_path, map_res_path_inner, quoted_strings, strip_quotes};
use crate::types::{FrameworkResolverExtractionResult, RefView};

/// The marker basename this parser handles.
pub(crate) const PROJECT_FILE_BASENAME: &str = "project.godot";

/// `true` when `file_path`'s basename is exactly `project.godot`.
pub(crate) fn is_project_godot(file_path: &str) -> bool {
    file_path
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base == PROJECT_FILE_BASENAME)
}

/// Parse a `project.godot` manifest into framework nodes + references.
///
/// Deterministic: nodes are emitted in source order; ids follow the upstream
/// `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}` formula via
/// [`generate_node_id`]. Lines are 1-based.
pub(crate) fn parse_project_godot(
    file_path: &str,
    content: &str,
    project_root: &str,
) -> FrameworkResolverExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<RefView> = Vec::new();

    // `run/main_scene` and `[autoload]` values may be a `uid://…` (Godot 4.x
    // default) rather than a `res://…` path. A scene uid resolves through the
    // project-wide `uid → .tscn path` map; an autoload SCRIPT uid resolves
    // through the `uid → .gd path` sidecar map. Both are built lazily on first
    // `uid://` need so a project without one pays nothing.
    let mut scene_uids: Option<BTreeMap<String, String>> = None;
    let mut script_uids: Option<BTreeMap<String, String>> = None;

    let mut section: Option<Section> = None;
    // Track whether we are inside a multi-line `key={ ... }` value so its inner
    // lines are not mistaken for new keys.
    let mut brace_depth: i32 = 0;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as i64;
        let line = raw_line.trim();

        // Inside a multi-line dictionary value: just track braces, skip.
        if brace_depth > 0 {
            brace_depth += brace_delta(raw_line);
            continue;
        }

        if line.is_empty() || is_comment(line) {
            continue;
        }

        // Section header `[name]`.
        if let Some(name) = parse_section_header(line) {
            section = Section::from_name(name);
            continue;
        }

        let Some(section) = section else {
            // Top-level keys (e.g. `config_version=5`) are not autoloads.
            continue;
        };

        let Some((key, value)) = split_key_value(line) else {
            // Malformed line (no `=`): skip, keep parsing.
            continue;
        };

        match section {
            Section::Autoload => emit_autoload(
                file_path,
                line_no,
                key,
                value,
                project_root,
                &mut scene_uids,
                &mut script_uids,
                &mut nodes,
                &mut references,
            ),
            Section::Application => {
                if key == "run/main_scene" {
                    emit_main_scene(
                        file_path,
                        line_no,
                        value,
                        project_root,
                        &mut scene_uids,
                        &mut nodes,
                        &mut references,
                    );
                }
            }
            Section::Input => {
                emit_input_action(file_path, line_no, key, &mut nodes);
                // A `key={` opens a (possibly multi-line) dictionary value.
                brace_depth += brace_delta(raw_line);
            }
            Section::EditorPlugins => {
                if key == "enabled" {
                    emit_enabled_plugins(file_path, line_no, value, &mut nodes, &mut references);
                }
            }
            Section::Other => {}
        }
    }

    FrameworkResolverExtractionResult { nodes, references }
}

/// Map each `[autoload]` singleton NAME to its repo-relative backing script
/// path, for L7's cross-file binding confirmation.
///
/// Reuses the exact `[autoload]` line scan + [`map_res_path`] rule
/// [`parse_project_godot`] uses, but yields only `(name, path)` pairs (no nodes
/// / no clock), so it is pure and deterministic. An entry whose value is not a
/// `res://` path is skipped. First-write-wins on a duplicate name.
pub(crate) fn autoload_script_paths(content: &str, project_root: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut section: Option<Section> = None;
    let mut brace_depth: i32 = 0;
    let mut script_uids: Option<BTreeMap<String, String>> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if brace_depth > 0 {
            brace_depth += brace_delta(raw_line);
            continue;
        }
        if line.is_empty() || is_comment(line) {
            continue;
        }
        if let Some(name) = parse_section_header(line) {
            section = Section::from_name(name);
            continue;
        }
        let Some(section) = section else {
            continue;
        };
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        match section {
            Section::Autoload => {
                if !key.is_empty() {
                    if let Some(path) = autoload_script_path(value, project_root, &mut script_uids)
                    {
                        if !out.iter().any(|(n, _)| n == key) {
                            out.push((key.to_string(), path));
                        }
                    }
                }
            }
            Section::Input => brace_depth += brace_delta(raw_line),
            _ => {}
        }
    }
    out
}

/// Resolve an `[autoload]` value to a repo-relative SCRIPT path for F1 binding.
/// A `res://…` value maps via [`map_res_path`]; a `uid://…` value resolves ONLY
/// through the `.gd.uid` sidecar map (a script autoload). A scene-backed uid
/// (`.tscn` header) yields `None` here — F1 binds to a script's own `func`s, and
/// a scene autoload has no such backing script (registration-only per the plan).
fn autoload_script_path(
    value: &str,
    project_root: &str,
    script_uids: &mut Option<BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(path) = map_res_path(value) {
        return Some(path);
    }
    let unquoted = strip_quotes(value).trim();
    let uid = unquoted.strip_prefix('*').unwrap_or(unquoted);
    if !uid.starts_with("uid://") {
        return None;
    }
    let scripts =
        script_uids.get_or_insert_with(|| super::godot_scene::gd_uid_sidecar_map(project_root));
    scripts.get(uid).cloned()
}

/// The sections L1 understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Autoload,
    Application,
    Input,
    EditorPlugins,
    Other,
}

impl Section {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "autoload" => Self::Autoload,
            "application" => Self::Application,
            "input" => Self::Input,
            "editor_plugins" => Self::EditorPlugins,
            _ => Self::Other,
        })
    }
}

/// Emit a singleton node + a `References` edge to its target script/scene.
///
/// A `res://…` value maps directly via [`map_res_path`]. A `uid://…` value
/// (Godot 4.x default) resolves through a combined uid lookup: the `.gd.uid`
/// SIDECAR map FIRST (a SCRIPT autoload), then the scene `uid → .tscn` map (a
/// scene-backed autoload). Either resolution emits the SAME
/// `Autoload`-subkind reference the `res://` form emits, so it enters the
/// reverse-consume lanes with zero query change. An unresolvable uid emits the
/// singleton node but NO reference (unknown-uid → no guess).
#[allow(clippy::too_many_arguments)]
fn emit_autoload(
    file_path: &str,
    line_no: i64,
    name: &str,
    value: &str,
    project_root: &str,
    scene_uids: &mut Option<BTreeMap<String, String>>,
    script_uids: &mut Option<BTreeMap<String, String>>,
    nodes: &mut Vec<Node>,
    references: &mut Vec<RefView>,
) {
    if name.is_empty() {
        return;
    }
    let node = constant_node(file_path, line_no, name);
    let node_id = node.id.clone();
    nodes.push(node);

    if let Some(target) = resolve_autoload_target(value, project_root, scene_uids, script_uids) {
        references.push(autoload_reference(node_id, target, line_no, file_path));
    }
}

/// Resolve an `[autoload]` value to a repo-relative target path. A `res://…`
/// value maps via the shared [`map_res_path`]; a `uid://…` value is resolved
/// through the `.gd.uid` sidecar map (script autoload) first, then the scene
/// `uid → .tscn` map (scene-backed autoload). Any other form yields `None`.
fn resolve_autoload_target(
    value: &str,
    project_root: &str,
    scene_uids: &mut Option<BTreeMap<String, String>>,
    script_uids: &mut Option<BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(target) = map_res_path(value) {
        return Some(target);
    }
    let unquoted = strip_quotes(value).trim();
    let uid = unquoted.strip_prefix('*').unwrap_or(unquoted);
    if !uid.starts_with("uid://") {
        return None;
    }
    let scripts =
        script_uids.get_or_insert_with(|| super::godot_scene::gd_uid_sidecar_map(project_root));
    if let Some(path) = scripts.get(uid) {
        return Some(path.clone());
    }
    let scenes = scene_uids.get_or_insert_with(|| super::godot_scene::scene_uid_map(project_root));
    scenes.get(uid).cloned()
}

/// Emit a main-scene marker node + a `References` edge to the scene path.
///
/// `run/main_scene` is either a `res://…` path (mapped directly) or, in Godot
/// 4.x by default, a `uid://…` handle that must be resolved through the
/// project-wide scene `uid → path` map (built lazily into `scene_uids` on first
/// need). Either way the emitted edge is an UNTAGGED `References` edge (subkind
/// `None`) — identical to the existing `res://` path, since the reverse-consume
/// lane (`resource_impact`) already surfaces untagged Godot `References` refs by
/// path. A `uid://…` with no mapped scene yields NO edge (unknown-uid → no
/// panic, no guess).
fn emit_main_scene(
    file_path: &str,
    line_no: i64,
    value: &str,
    project_root: &str,
    scene_uids: &mut Option<std::collections::BTreeMap<String, String>>,
    nodes: &mut Vec<Node>,
    references: &mut Vec<RefView>,
) {
    let Some(target) = resolve_main_scene(value, project_root, scene_uids) else {
        return;
    };
    let node = constant_node(file_path, line_no, "main_scene");
    let node_id = node.id.clone();
    nodes.push(node);
    references.push(reference(node_id, target, line_no, file_path));
}

/// Resolve a `run/main_scene` value to a repo-relative scene path. A `res://…`
/// value maps via the shared [`map_res_path`]; a `uid://…` value is looked up in
/// the lazily-built scene uid map. Any other form yields `None`.
fn resolve_main_scene(
    value: &str,
    project_root: &str,
    scene_uids: &mut Option<std::collections::BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(target) = map_res_path(value) {
        return Some(target);
    }
    let uid = super::godot_common::strip_quotes(value).trim();
    if !uid.starts_with("uid://") {
        return None;
    }
    let map = scene_uids.get_or_insert_with(|| super::godot_scene::scene_uid_map(project_root));
    map.get(uid).cloned()
}

/// Emit a node per input action name (the key). The value (a dictionary
/// literal) is intentionally not parsed at L1.
fn emit_input_action(file_path: &str, line_no: i64, name: &str, nodes: &mut Vec<Node>) {
    if name.is_empty() {
        return;
    }
    nodes.push(constant_node(file_path, line_no, name));
}

/// Emit a `References` edge per enabled plugin path inside
/// `PackedStringArray("res://...", ...)`.
fn emit_enabled_plugins(
    file_path: &str,
    line_no: i64,
    value: &str,
    nodes: &mut Vec<Node>,
    references: &mut Vec<RefView>,
) {
    let mut emitted_any = false;
    let mut node_id: Option<String> = None;
    for quoted in quoted_strings(value) {
        let Some(target) = map_res_path_inner(quoted) else {
            continue;
        };
        if !emitted_any {
            let node = constant_node(file_path, line_no, "editor_plugins");
            node_id = Some(node.id.clone());
            nodes.push(node);
            emitted_any = true;
        }
        if let Some(id) = &node_id {
            references.push(reference(id.clone(), target, line_no, file_path));
        }
    }
}

/// Build a [`NodeKind::Constant`] node with the deterministic upstream id.
fn constant_node(file_path: &str, line_no: i64, name: &str) -> Node {
    let id = generate_node_id(file_path, NodeKind::Constant, name, line_no as u32);
    framework_node(
        id,
        NodeKind::Constant,
        name.to_string(),
        format!("{file_path}::{name}"),
        file_path.to_string(),
        line_no,
        line_no,
        0,
        0,
        Language::GodotProject,
        false,
    )
}

/// Build a `References` edge from a synthesized node to a repo-relative path.
fn reference(from_node_id: String, target: String, line_no: i64, file_path: &str) -> RefView {
    RefView {
        from_node_id,
        reference_name: target,
        reference_kind: EdgeKind::References,
        line: line_no,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::GodotProject,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// Like [`reference`] but tags the ref `ReferenceSubkind::Autoload`. The
/// autoload `project.godot` -> `res://x.gd` ref crosses a language-family
/// boundary (GodotProject and GDScript both `language_family() == None`), so
/// `gate_language` drops its resolution and it stays UNRESOLVED; `resource_impact`
/// then reads the subkind from `reference.reference_subkind` — so tagging the
/// `RefView` here is the correct and sufficient layer (the resolved-edge
/// `build_edge_metadata` path never fires for autoload). Used ONLY by
/// `emit_autoload`; `reference` stays `None` for the main-scene and
/// enabled-plugin refs that share it.
fn autoload_reference(
    from_node_id: String,
    target: String,
    line_no: i64,
    file_path: &str,
) -> RefView {
    RefView {
        reference_subkind: Some(ReferenceSubkind::Autoload),
        ..reference(from_node_id, target, line_no, file_path)
    }
}

// ---------------------------------------------------------------------------
// Line-level parsing helpers
// ---------------------------------------------------------------------------

fn is_comment(line: &str) -> bool {
    line.starts_with(';') || line.starts_with('#')
}

/// `[name]` → `Some("name")`. Requires the trimmed line to start `[` and end
/// `]` with no interior `]`.
fn parse_section_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    if inner.contains(']') || inner.is_empty() {
        return None;
    }
    Some(inner)
}

/// Split `key=value` at the FIRST `=`. Returns `None` when there is no `=`.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    let value = line[eq + 1..].trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Net `{` minus `}` count on a line (string-content unaware, sufficient for
/// the brace-balanced dictionary values in `[input]`).
fn brace_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    for b in line.bytes() {
        match b {
            b'{' => delta += 1,
            b'}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoload_script_paths_maps_name_to_repo_relative() {
        let content =
            "[autoload]\nGameState=\"*res://globals/state.gd\"\nMusic=\"res://audio/m.gd\"\n";
        let got = autoload_script_paths(content, "");
        assert_eq!(
            got,
            vec![
                ("GameState".to_string(), "globals/state.gd".to_string()),
                ("Music".to_string(), "audio/m.gd".to_string()),
            ]
        );
    }

    #[test]
    fn autoload_script_paths_first_write_wins_on_dup_name() {
        let content = "[autoload]\nX=\"res://a.gd\"\nX=\"res://b.gd\"\n";
        let got = autoload_script_paths(content, "");
        assert_eq!(got, vec![("X".to_string(), "a.gd".to_string())]);
    }

    #[test]
    fn autoload_script_paths_skips_non_res_value() {
        let content = "[autoload]\nX=\"user://a.gd\"\n";
        assert!(autoload_script_paths(content, "").is_empty());
    }

    #[test]
    fn autoload_script_paths_ignores_input_multiline_and_other_sections() {
        let content = "\
[input]
jump={
\"deadzone\": 0.5
}

[application]
run/main_scene=\"res://main.tscn\"

[autoload]
X=\"res://x.gd\"
";
        assert_eq!(
            autoload_script_paths(content, ""),
            vec![("X".to_string(), "x.gd".to_string())]
        );
    }

    #[test]
    fn autoload_script_paths_empty_when_no_autoload() {
        assert!(autoload_script_paths("; comment\nconfig_version=5\n", "").is_empty());
    }

    #[test]
    fn resolve_main_scene_maps_res_path_without_uid_map() {
        let mut uids: Option<std::collections::BTreeMap<String, String>> = None;
        let got = resolve_main_scene("\"res://main.tscn\"", "", &mut uids);
        assert_eq!(got.as_deref(), Some("main.tscn"));
        assert!(uids.is_none(), "res:// path must not build the uid map");
    }

    #[test]
    fn resolve_main_scene_non_res_non_uid_is_none() {
        let mut uids: Option<std::collections::BTreeMap<String, String>> = None;
        assert!(resolve_main_scene("\"user://x.tscn\"", "", &mut uids).is_none());
        assert!(uids.is_none());
    }

    #[test]
    fn emit_main_scene_uid_resolves_via_scene_map() {
        let dir = std::env::temp_dir().join(format!(
            "cg-mainscene-uid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("Scenes/MainMenu")).expect("mkdir");
        std::fs::write(
            dir.join("Scenes/MainMenu/main_menu.tscn"),
            "[gd_scene load_steps=2 format=3 uid=\"uid://abc123\"]\n\n[node name=\"Root\" type=\"Node\"]\n",
        )
        .expect("write tscn");
        let root = dir.to_string_lossy().to_string();

        let content = "config_version=5\n\n[application]\nrun/main_scene=\"uid://abc123\"\n";
        let result = parse_project_godot("project.godot", content, &root);

        assert_eq!(result.references.len(), 1, "one main_scene ref emitted");
        let r = &result.references[0];
        assert_eq!(r.reference_name, "Scenes/MainMenu/main_menu.tscn");
        assert_eq!(r.reference_kind, EdgeKind::References);
        assert_eq!(
            r.reference_subkind, None,
            "uid main_scene ref must be untagged, like the res:// path"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn emit_main_scene_unknown_uid_yields_no_ref() {
        let dir = std::env::temp_dir().join(format!(
            "cg-mainscene-uid-miss-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let root = dir.to_string_lossy().to_string();

        let content = "config_version=5\n\n[application]\nrun/main_scene=\"uid://missing\"\n";
        let result = parse_project_godot("project.godot", content, &root);
        assert!(
            result.references.is_empty(),
            "unknown uid must emit no ref, got: {:?}",
            result.references
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn autoload_temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-autoload-uid-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn emit_autoload_uid_sidecar_form_emits_reference() {
        let dir = autoload_temp_dir("sidecar");
        std::fs::create_dir_all(dir.join("Scripts/Autoloads")).unwrap();
        std::fs::write(
            dir.join("Scripts/Autoloads/effect_manager.gd"),
            "extends Node\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Scripts/Autoloads/effect_manager.gd.uid"),
            "uid://bb1ewunp0bc47\n",
        )
        .unwrap();
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nEffectManager=\"*uid://bb1ewunp0bc47\"\n";
        let result = parse_project_godot("project.godot", content, &root);

        assert_eq!(result.references.len(), 1, "one autoload ref emitted");
        let r = &result.references[0];
        assert_eq!(r.reference_name, "Scripts/Autoloads/effect_manager.gd");
        assert_eq!(r.reference_subkind, Some(ReferenceSubkind::Autoload));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn emit_autoload_uid_scene_form_emits_reference() {
        let dir = autoload_temp_dir("scene");
        std::fs::create_dir_all(dir.join("Entities/UI")).unwrap();
        std::fs::write(
            dir.join("Entities/UI/ComboUI.tscn"),
            "[gd_scene format=3 uid=\"uid://bddoi8q2q2olp\"]\n",
        )
        .unwrap();
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nComboUi=\"*uid://bddoi8q2q2olp\"\n";
        let result = parse_project_godot("project.godot", content, &root);

        assert_eq!(result.references.len(), 1, "one autoload ref emitted");
        let r = &result.references[0];
        assert_eq!(r.reference_name, "Entities/UI/ComboUI.tscn");
        assert_eq!(r.reference_subkind, Some(ReferenceSubkind::Autoload));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gd_uid_collision_sidecar_wins_over_scene() {
        let dir = autoload_temp_dir("collision");
        std::fs::create_dir_all(dir.join("Scripts")).unwrap();
        std::fs::write(dir.join("Scripts/effect_manager.gd"), "extends Node\n").unwrap();
        std::fs::write(dir.join("Scripts/effect_manager.gd.uid"), "uid://collide\n").unwrap();
        std::fs::write(
            dir.join("Scripts/ComboUI.tscn"),
            "[gd_scene format=3 uid=\"uid://collide\"]\n",
        )
        .unwrap();
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nShared=\"*uid://collide\"\n";
        let result = parse_project_godot("project.godot", content, &root);

        assert_eq!(result.references.len(), 1, "one autoload ref emitted");
        assert_eq!(
            result.references[0].reference_name, "Scripts/effect_manager.gd",
            "on a uid present in BOTH the sidecar and the scene map, the sidecar .gd wins"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn emit_autoload_unknown_uid_no_reference() {
        let dir = autoload_temp_dir("miss");
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nGhost=\"*uid://nope\"\n";
        let result = parse_project_godot("project.godot", content, &root);

        assert_eq!(result.nodes.len(), 1, "singleton node still emitted");
        assert!(
            result.references.is_empty(),
            "unknown uid emits no reference, got: {:?}",
            result.references
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autoload_script_paths_resolves_uid_sidecar_form() {
        let dir = autoload_temp_dir("scriptpaths");
        std::fs::create_dir_all(dir.join("Scripts")).unwrap();
        std::fs::write(dir.join("Scripts/effect_manager.gd"), "extends Node\n").unwrap();
        std::fs::write(
            dir.join("Scripts/effect_manager.gd.uid"),
            "uid://bb1ewunp0bc47\n",
        )
        .unwrap();
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nEffectManager=\"*uid://bb1ewunp0bc47\"\n";
        let got = autoload_script_paths(content, &root);
        assert_eq!(
            got,
            vec![(
                "EffectManager".to_string(),
                "Scripts/effect_manager.gd".to_string()
            )]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autoload_script_paths_skips_scene_backed_uid() {
        let dir = autoload_temp_dir("scriptpaths-scene");
        std::fs::create_dir_all(dir.join("Entities")).unwrap();
        std::fs::write(
            dir.join("Entities/ComboUI.tscn"),
            "[gd_scene format=3 uid=\"uid://bddoi8q2q2olp\"]\n",
        )
        .unwrap();
        let root = dir.to_string_lossy().to_string();

        let content = "[autoload]\nComboUi=\"*uid://bddoi8q2q2olp\"\n";
        assert!(
            autoload_script_paths(content, &root).is_empty(),
            "scene-backed uid autoload has no F1 script binding"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
