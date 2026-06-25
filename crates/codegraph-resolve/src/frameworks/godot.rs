//! Godot [`FrameworkResolver`] — SKELETON (T2 of godot-static-analysis).
//!
//! This file ships detection + registration ONLY. Every semantic method
//! (`resolve` / `claims_reference` / `extract` / `post_extract`) is a documented
//! stub returning the "absent" value; the real Godot parsing and dynamic-edge
//! synthesis land in later layers:
//!
//! - T3/T4/T5: [`GodotResolver::extract`] parses `project.godot` (autoload /
//!   input map), `.tscn` (node tree, ext_resource, connections, groups), and
//!   `.tres` (resource → script/resource refs) into nodes + references.
//! - T6: [`GodotResolver::claims_reference`] + [`GodotResolver::resolve`] handle
//!   dynamic GDScript dispatch (signal connect/emit, `get_node`/`$`/`%`,
//!   `get_nodes_in_group`, autoload singleton access).
//! - T7: [`GodotResolver::post_extract`] finalizes the cross-file scene↔script
//!   binding, autoload graph, and signal-connection targets.
//!
//! # Parser-strategy decision (spike result, recorded here for T3-T5)
//!
//! T3/T4/T5 will parse `.tscn` / `.tres` / `project.godot` with a HAND parser
//! (regex + manual byte scanning), NOT a tree-sitter grammar, and will add NO
//! dependency. Rationale:
//!
//! - The Godot resource format is a flat, regular text grammar: `[section]`
//!   headers, `key = value` lines, and `ExtResource("id")` / `SubResource("id")`
//!   handles. It needs no recursive descent — exactly the shape the existing
//!   custom extractors already handle by hand.
//! - It matches the in-tree precedent: `embedded/liquid.rs` and
//!   `embedded/mybatis.rs` extract with `regex` + byte scanning and zero grammar
//!   crate (mybatis already parses `<section>`-like XML this way; liquid scans
//!   `{% %}` tags and JSON sections). The nestjs resolver in this very crate
//!   likewise uses a balanced-paren byte scanner (`read_args`) rather than a
//!   grammar.
//! - Adding `tree-sitter-godot-resource` would introduce a new grammar ABI +
//!   guardrail surface for no robustness gain on a format this regular, and the
//!   project's no-new-dep posture (golden byte-stability + guardrail) favors the
//!   hand parser. Adopt the grammar ONLY if a later layer hits a concrete
//!   robustness wall the hand parser cannot clear (e.g. deeply nested quoted /
//!   escaped values inside resource literals); none is evident in the format.

use codegraph_core::types::{Language, Node};

use super::godot_project;
use super::godot_resource;
use super::godot_scene;
use super::godot_script;
use crate::framework::FrameworkResolver;
use crate::types::{FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedRef};

/// The marker file whose presence at the project root defines a Godot project.
const PROJECT_FILE: &str = "project.godot";

/// Godot resolver (T2 skeleton). Owns all Godot semantics once T3-T7 fill the
/// stubs; for now it only detects a Godot project and registers itself.
pub struct GodotResolver;

impl FrameworkResolver for GodotResolver {
    fn name(&self) -> &str {
        "godot"
    }

    /// `None` = applies to ALL languages. The Godot inputs the later layers must
    /// see — `project.godot`, `.tscn`, `.tres` (each now a `Language` variant
    /// from T1) plus `.gd` (`Language::Gdscript`) — span multiple `Language`s,
    /// and `extract()`/`resolve()` need to inspect every one of them. Returning
    /// the T1 Godot variants only would exclude `.gd`; returning all four would
    /// still gate out the cross-file passes that want unrestricted file access.
    /// `None` keeps the resolver applicable everywhere, with `detect()` (gated
    /// strictly on `project.godot`) as the real activation guard.
    fn languages(&self) -> Option<&[Language]> {
        None
    }

    /// A project IS a Godot project iff it has a `project.godot` at its root.
    /// Paths in the [`ResolutionContext`] are project-relative (the nestjs
    /// resolver reads `"package.json"` the same way), so the root marker is the
    /// bare relative path `project.godot`. `file_exists` is the cleanest query
    /// for "is this exact file present"; `get_all_files` would also work but
    /// would need a basename scan. Guarding strictly on the marker prevents the
    /// resolver (whose `languages()` is `None`) from activating on every project.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context.file_exists(PROJECT_FILE)
    }

    /// STUB — dynamic-reference resolution lands in T6 (signal connect/emit,
    /// `get_node`/`$`/`%`, group queries, autoload singleton access).
    fn resolve(
        &self,
        _reference: &RefView,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }

    /// STUB — dynamic-dispatch name opt-in lands in T6 alongside [`Self::resolve`].
    fn claims_reference(&self, _name: &str) -> bool {
        false
    }

    /// Per-file Godot parsing dispatch.
    ///
    /// L1 (T3): when the file's basename is `project.godot`, delegate to
    /// [`godot_project::parse_project_godot`] (autoload-singleton graph + input
    /// actions + main scene + enabled plugins).
    ///
    /// L2 (T4): when the file's basename ends in `.tscn`, delegate to
    /// [`godot_scene::parse_tscn`] (scene-tree nodes + script-binding,
    /// signal-handler, group-membership, and instanced-subscene references).
    ///
    /// L4 (T5): when the file's basename ends in `.tres`, delegate to
    /// [`godot_resource::parse_tres`] (a single resource marker node +
    /// resource→script / resource→resource references resolved through the same
    /// `[ext_resource]` id-table mechanics as L2).
    ///
    /// L3 (T6): when the file's basename ends in `.gd`, delegate to
    /// [`godot_script::parse_gdscript_dynamics`] (dynamic GDScript call-sites:
    /// `connect`/`emit_signal`/`get_node`/`$`/`%`/group queries/`has_method`/
    /// `call` → references to the literal target NAME, or a dynamic-unresolved
    /// sentinel reference when the target is a computed/non-literal expression).
    /// This is ADDITIVE and gated behind [`Self::detect`] (a `project.godot`
    /// must exist), so a plain non-Godot `.gd` repo never reaches here and the
    /// golden-protected base GDScript symbol extraction is untouched.
    ///
    /// Every other file returns `None`, which the pipeline
    /// (`extract_and_persist_frameworks`) treats as "this resolver has nothing
    /// for this file".
    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkResolverExtractionResult> {
        if godot_project::is_project_godot(file_path) {
            return Some(godot_project::parse_project_godot(file_path, content));
        }
        if godot_scene::is_tscn(file_path) {
            return Some(godot_scene::parse_tscn(file_path, content));
        }
        if godot_resource::is_tres(file_path) {
            return Some(godot_resource::parse_tres(file_path, content));
        }
        if godot_script::is_gdscript(file_path) {
            return Some(godot_script::parse_gdscript_dynamics(file_path, content));
        }
        None
    }

    /// STUB — cross-file scene↔script binding, autoload graph, and
    /// signal-connection finalization land in T7.
    fn post_extract(&self, _context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        None
    }
}
