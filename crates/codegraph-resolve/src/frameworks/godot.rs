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

use codegraph_core::types::{Language, Node, NodeKind};

use super::godot_project;
use super::godot_resource;
use super::godot_scene;
use super::godot_script;
use crate::framework::FrameworkResolver;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

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

    /// Autoload-singleton access resolution (the headline L7 capability).
    ///
    /// The base GDScript extractor already emits a `BuffManager.apply()`
    /// member-call as a `Calls` reference named `BuffManager.apply` (receiver
    /// `BuffManager`, method `apply`). The generic name-matcher CANNOT bind it
    /// to the autoload singleton: that node is a [`NodeKind::Constant`] (not a
    /// `Class`/`Struct`/`Interface`), so the matcher's class/instance strategies
    /// skip it. This resolver step is the seam that closes that gap — it is the
    /// ONLY edge-producing path for autoload access (`post_extract` returns
    /// nodes only, never edges).
    ///
    /// GATED BY THE KNOWN AUTOLOAD NAME SET → zero false positives. The receiver
    /// (`BuffManager`) must EXACTLY match a `[autoload]` singleton name declared
    /// in `project.godot` (L1 emitted those as `NodeKind::Constant` /
    /// `Language::GodotProject` nodes). A receiver that is NOT a known autoload
    /// (`Vector2.ZERO`, `Input.is_action_pressed`, a local variable) is left for
    /// the generic pass — this resolver returns `None` and never fabricates an
    /// edge. The L3 [`godot_script`] layer deliberately deferred emitting these
    /// candidate references precisely because only here (with the autoload roster
    /// in hand) can they be matched safely.
    ///
    /// The resolved edge points the using symbol at the autoload SINGLETON node
    /// (which itself carries a `References` edge to its backing script, so the
    /// onward script link is reachable via one hop). Confidence is 0.9 so it
    /// short-circuits Strategy 1 in `resolve_one_pure` before the name-matcher,
    /// guaranteeing no competing/duplicate edge.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.language != Language::Gdscript
            || reference
                .reference_name
                .starts_with(godot_script::DYNAMIC_PREFIX)
        {
            return None;
        }
        let receiver = match reference.reference_name.split_once('.') {
            Some((head, _)) => head,
            None => reference.reference_name.as_str(),
        };
        if receiver.is_empty() {
            return None;
        }
        let singleton = find_autoload_singleton(context, receiver)?;
        Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: singleton.id,
            confidence: 0.9,
            resolved_by: ResolvedBy::Framework,
        })
    }

    /// Opt a `BuffManager.member` reference through the name-exists pre-filter.
    ///
    /// The generic pre-filter ([`crate::resolver`]'s `has_any_possible_match`)
    /// already passes a `Receiver.member` reference whose RECEIVER is a known
    /// node name, and an autoload singleton IS a known node (named `BuffManager`,
    /// emitted by L1), so the common autoload-call shape reaches [`Self::resolve`]
    /// without help here. This hook is intentionally conservative: it returns
    /// `false`, because claiming a NAME blindly (without the context needed to
    /// check it against the autoload roster) would let arbitrary non-autoload
    /// member calls through the pre-filter only to be rejected by `resolve`,
    /// which is wasteful and risks surprising interactions with the other
    /// strategies. The roster-gated check lives in `resolve`, where the context
    /// is available.
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
    /// sentinel reference when the target is a computed/non-literal expression),
    /// and ALSO merge [`godot_script::parse_autoload_candidates`] (the T7
    /// autoload layer: `Uppercase.member` accesses emitted as candidate
    /// references that [`Self::resolve`] roster-gates against the known autoload
    /// singletons — the base GDScript extractor emits no such call references, so
    /// this is the only source of an autoload-access reference). Both are
    /// ADDITIVE and gated behind [`Self::detect`] (a `project.godot` must exist),
    /// so a plain non-Godot `.gd` repo never reaches here and the
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
            let mut result = godot_script::parse_gdscript_dynamics(file_path, content);
            let candidates = godot_script::parse_autoload_candidates(file_path, content);
            result.references.extend(candidates.references);
            return Some(result);
        }
        None
    }

    /// Cross-file finalization pass (L7), run once after every file's
    /// `extract()` and after the generic resolution pass.
    ///
    /// Per the [`FrameworkResolver::post_extract`] contract this returns NODES
    /// (mutated, id preserved), persisted via `upsert_nodes` — it is NOT an
    /// edge-producing pass. The Godot cross-file work splits cleanly along that
    /// line:
    ///
    /// - AUTOLOAD ACCESS (`BuffManager.apply()` → the singleton) is an EDGE, so
    ///   it is produced in [`Self::resolve`] during the generic pass (the only
    ///   edge seam), gated by the autoload name set. It is NOT redone here.
    /// - SCENE→SCRIPT binding (`.tscn` `script = ExtResource(...)` → the
    ///   `player.gd` path) and SIGNAL-CONNECTION handlers (`.tscn`
    ///   `[connection method="_on_x"]` → `func _on_x`) are EDGES the L2 layer
    ///   already emits as path/name references; the generic resolution pass
    ///   resolves those path/name references to the script file node and the
    ///   handler function symbol respectively. No autoload-roster-style gating
    ///   is needed for them, so they need no `post_extract` work and are not
    ///   duplicated here.
    ///
    /// What genuinely needs the post-resolution, whole-graph view is the
    /// AUTOLOAD SINGLETON's own metadata: L1 emitted each singleton as a bare
    /// `NodeKind::Constant` named after the autoload (`BuffManager`) with a
    /// `qualified_name` of `project.godot::BuffManager`. Only after every file is
    /// indexed can we confirm the singleton's backing script actually exists in
    /// the store and stamp the resolved repo-relative script path into the
    /// singleton's `signature` (id + name + qualified_name preserved, so the
    /// pass is idempotent and existing edges stay intact). This gives the
    /// MCP/CLI layer (T8) a confirmed singleton→script binding to surface
    /// without re-deriving it, and is the analogue of NestJS stamping the
    /// resolved route prefix onto its route nodes.
    ///
    /// A singleton whose script path is NOT present in the store is left
    /// untouched (no fabricated binding). Returns only the nodes actually
    /// changed.
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let bindings = autoload_script_bindings(context);
        if bindings.is_empty() {
            return Some(Vec::new());
        }
        let mut updates: Vec<Node> = Vec::new();
        for singleton in autoload_singletons(context) {
            let Some(script_path) = bindings.get(&singleton.name) else {
                continue;
            };
            if !context.file_exists(script_path) {
                continue;
            }
            let confirmed = format!("autoload -> {script_path}");
            if singleton.signature.as_deref() == Some(confirmed.as_str()) {
                continue;
            }
            let mut updated = singleton;
            updated.signature = Some(confirmed);
            updated.updated_at = super::now_millis();
            updates.push(updated);
        }
        Some(updates)
    }
}

/// The autoload singleton nodes L1 emitted: `NodeKind::Constant` nodes whose
/// `file_path` basename is `project.godot`. Filtering on the file restricts the
/// broad `Constant` kind to L1's `project.godot` markers (autoload / main-scene
/// / input / plugin); the `autoload_script_bindings` map then keeps only the
/// `[autoload]` entries that have a backing script path.
fn autoload_singletons(context: &dyn ResolutionContext) -> Vec<Node> {
    context
        .get_nodes_by_kind(NodeKind::Constant)
        .into_iter()
        .filter(|n| {
            n.language == Language::GodotProject && godot_project::is_project_godot(&n.file_path)
        })
        .collect()
}

/// Find the autoload singleton node whose name EXACTLY equals `name`.
///
/// This is the roster gate for [`GodotResolver::resolve`]: a receiver only
/// resolves when it names a real `[autoload]` singleton. Scans the `Constant`
/// nodes restricted to `project.godot` so it cannot match an unrelated constant.
fn find_autoload_singleton(context: &dyn ResolutionContext, name: &str) -> Option<Node> {
    context.get_nodes_by_name(name).into_iter().find(|n| {
        n.kind == NodeKind::Constant
            && n.language == Language::GodotProject
            && godot_project::is_project_godot(&n.file_path)
    })
}

/// Map of `autoload name -> repo-relative script path`, recovered by RE-READING
/// every `project.godot` in the project and re-parsing its `[autoload]` section.
///
/// The [`ResolutionContext`] exposes no outgoing-edge accessor, so the singleton
/// → script path cannot be read back off the L1 `References` edge. Re-reading the
/// source file is the same technique the NestJS `post_extract` uses to recover
/// `RouterModule` registrations, and it is deterministic. Only `[autoload]`
/// entries whose value maps to a `res://` script path are kept; the L1 parser's
/// emitted nodes are reused to identify which `project.godot` files to read.
fn autoload_script_bindings(
    context: &dyn ResolutionContext,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut seen_files = std::collections::BTreeSet::new();
    for file in context.get_all_files() {
        if !godot_project::is_project_godot(&file) {
            continue;
        }
        if !seen_files.insert(file.clone()) {
            continue;
        }
        let Some(content) = context.read_file(&file) else {
            continue;
        };
        for (name, path) in godot_project::autoload_script_paths(&content) {
            out.entry(name).or_insert(path);
        }
    }
    out
}
