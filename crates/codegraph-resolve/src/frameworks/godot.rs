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

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

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
        if reference.language != Language::Gdscript {
            return None;
        }

        // F1: an autoload-method-call candidate (`godot:autoload_call:Recv.member`).
        // Roster-gated resolution to the UNIQUE same-named `func` in the receiver's
        // bound script; returns `None` (no edge) unless exactly one func matches.
        if let Some(access) = reference
            .reference_name
            .strip_prefix(godot_script::AUTOLOAD_CALL_PREFIX)
        {
            if reference.reference_kind == EdgeKind::Calls {
                if let Some(target) = resolve_autoload_func(context, access) {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: target.id,
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Framework,
                    });
                }
            }
            return None;
        }

        if reference
            .reference_name
            .starts_with(godot_script::DYNAMIC_PREFIX)
        {
            return None;
        }
        let (receiver, member) = match reference.reference_name.split_once('.') {
            Some((head, tail)) => (head, Some(tail)),
            None => (reference.reference_name.as_str(), None),
        };
        if receiver.is_empty() {
            return None;
        }

        // `ClassName.member()` static-call resolution (T2). A GDScript
        // `class_name X` global's members are file-level `Function` nodes in the
        // SAME file as the `Class` node (`class_name_statement` is NOT pushed on
        // the walker's node_stack, so `func`s stay top-level Functions). When the
        // RECEIVER matches a real GDScript `Class` node name and the reference is
        // a `Calls` ref shaped `<Class>.<member>`, resolve to the `Function`
        // named `<member>` in that class's file. Keyed ONLY on persisted fields
        // (kind/name/file_path) — no extraction marker. STATIC-ONLY: it fires
        // solely for a genuine class-node receiver, so a lowercase/instance/self
        // receiver never matches (no lowercase name is a `Class` node), and it
        // is checked BEFORE the autoload roster so a class global takes
        // precedence over an identically-named autoload constant.
        if reference.reference_kind == EdgeKind::Calls {
            if let Some(member) = member {
                if !member.is_empty() {
                    if let Some(target) = resolve_class_member(context, receiver, member) {
                        return Some(ResolvedRef {
                            original: reference.clone(),
                            target_node_id: target.id,
                            confidence: 0.9,
                            resolved_by: ResolvedBy::Framework,
                        });
                    }
                }
            }
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
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        project_root: &str,
    ) -> Option<FrameworkResolverExtractionResult> {
        if godot_project::is_project_godot(file_path) {
            return Some(godot_project::parse_project_godot(
                file_path,
                content,
                project_root,
            ));
        }
        if godot_scene::is_tscn(file_path) {
            return Some(godot_scene::parse_tscn(file_path, content));
        }
        if godot_resource::is_tres(file_path) {
            return Some(godot_resource::parse_tres(file_path, content, project_root));
        }
        if godot_script::is_gdscript(file_path) {
            let mut result = godot_script::parse_gdscript_dynamics(file_path, content);
            let candidates = godot_script::parse_autoload_candidates(file_path, content);
            result.references.extend(candidates.references);
            let func_candidates = godot_script::parse_autoload_func_candidates(file_path, content);
            result.references.extend(func_candidates.references);
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

/// Resolve a GDScript `<Class>.<member>` static call to the `Function` named
/// `member` in the class's own file.
///
/// The receiver must name a real GDScript `Class` node (a `class_name X`
/// global — lowercase/instance/self receivers never match). If the same class
/// NAME maps to multiple files (illegal in Godot but possible in a broken
/// project), the candidate files are sorted lexicographically and the first is
/// chosen, so output is byte-stable; `member` is resolved ONLY within that
/// file. If that file holds more than one `Function` named `member`, none is
/// resolved (ambiguity is left unresolved rather than emitting a guess).
fn resolve_class_member(
    context: &dyn ResolutionContext,
    receiver: &str,
    member: &str,
) -> Option<Node> {
    let mut class_files: Vec<String> = context
        .get_nodes_by_name(receiver)
        .into_iter()
        .filter(|n| n.kind == NodeKind::Class && n.language == Language::Gdscript)
        .map(|n| n.file_path)
        .collect();
    if class_files.is_empty() {
        return None;
    }
    class_files.sort();
    class_files.dedup();
    let class_file = &class_files[0];

    let mut matches = context
        .get_nodes_in_file(class_file)
        .into_iter()
        .filter(|n| {
            n.kind == NodeKind::Function && n.language == Language::Gdscript && n.name == member
        });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first)
}

/// Resolve an autoload method call `access` (`Receiver.member`) to the UNIQUE
/// same-named `func` in the receiver's bound script (F1). Returns `None` — no
/// edge — unless every determinism rule holds:
///
/// 1. Single binding source: the target script is the path bound to `Receiver`
///    in `project.godot`'s `[autoload]` section (via
///    [`autoload_script_bindings`]) — a `res://` `.gd` path or a sidecar-UID
///    (`*.gd.uid`) script autoload, both of which get full F1 binding. A
///    scene-header-UID autoload is registration-only (bound to a `.tscn`, no F1
///    binding to its attached script). A receiver that is not a real script-bound
///    autoload (a built-in like `Vector2`, a scene-backed autoload, a class
///    global) has no binding here → `None`. No global cross-file matching.
/// 2. Unique-candidate-only: `member` is searched for as a GDScript `Function`
///    ONLY inside that one bound script. Exactly one match → resolve to it; zero
///    or two-or-more matches → `None` (ambiguity/absence left unresolved, never
///    guessed).
fn resolve_autoload_func(context: &dyn ResolutionContext, access: &str) -> Option<Node> {
    let (receiver, member) = access.split_once('.')?;
    if receiver.is_empty() || member.is_empty() {
        return None;
    }
    let bindings = autoload_script_bindings(context);
    let script_path = bindings.get(receiver)?;

    let mut matches = context
        .get_nodes_in_file(script_path)
        .into_iter()
        .filter(|n| {
            n.kind == NodeKind::Function && n.language == Language::Gdscript && n.name == member
        });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first)
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
        for (name, path) in
            godot_project::autoload_script_paths(&content, context.get_project_root())
        {
            out.entry(name).or_insert(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Unit tests for the F1 autoload-resolution DEFENSIVE branches of
    //! `GodotResolver::resolve` / `resolve_autoload_func` / `resolve_class_member`
    //! / `autoload_script_bindings` / `post_extract`. A hand-rolled in-memory
    //! [`ResolutionContext`] (`Ctx`) lets each malformed / ambiguous / absent
    //! input be constructed exactly, so every early-`None` and skip path is hit
    //! with a meaningful behavioral assertion (no edge fabricated, no guess).
    use super::*;
    use crate::types::{ImportMapping, RefView};
    use codegraph_core::types::Language;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
    }

    impl Ctx {
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }
        fn node(mut self, n: Node) -> Self {
            self.nodes.push(n);
            self
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == q)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            let mut files: Vec<String> = self.files.keys().cloned().collect();
            files.sort();
            files
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn gd_node(id: &str, kind: NodeKind, name: &str, file: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language: lang,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn gd_ref(name: &str, kind: EdgeKind) -> RefView {
        RefView {
            from_node_id: "from:caller.gd".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 4,
            column: 0,
            file_path: "caller.gd".to_string(),
            language: Language::Gdscript,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    /// A `project.godot` whose `[autoload]` binds `GameFlow` to `game_flow.gd`.
    fn project_with_gameflow() -> String {
        "config_version=5\n\n[autoload]\n\nGameFlow=\"*res://game_flow.gd\"\n".to_string()
    }

    #[test]
    fn resolve_non_gdscript_reference_is_none() {
        let ctx = Ctx::default();
        let mut r = gd_ref("X.y", EdgeKind::Calls);
        r.language = Language::TypeScript;
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_autoload_call_prefix_with_no_func_returns_none() {
        // An `autoload_call:` prefixed ref whose bound script has NO matching
        // func → resolve_autoload_func yields None → resolve returns None
        // (never falls through to the singleton path for a prefixed name).
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .node(gd_node(
                "function:other",
                NodeKind::Function,
                "other",
                "game_flow.gd",
                Language::Gdscript,
            ));
        let name = format!(
            "{}GameFlow.return_to_map",
            godot_script::AUTOLOAD_CALL_PREFIX
        );
        let r = gd_ref(&name, EdgeKind::Calls);
        assert!(
            GodotResolver.resolve(&r, &ctx).is_none(),
            "no same-named func → no edge, and a prefixed ref never uses the singleton path"
        );
    }

    #[test]
    fn resolve_autoload_call_prefix_but_not_calls_kind_returns_none() {
        // A prefixed ref whose kind is NOT Calls skips the func lookup and
        // returns None at the end of the prefix block.
        let ctx = Ctx::default().file("project.godot", &project_with_gameflow());
        let name = format!(
            "{}GameFlow.return_to_map",
            godot_script::AUTOLOAD_CALL_PREFIX
        );
        let r = gd_ref(&name, EdgeKind::References);
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_dynamic_prefix_reference_is_none() {
        // A `godot:dynamic:*` sentinel is never resolvable by construction.
        let ctx = Ctx::default();
        let name = format!("{}connect", godot_script::DYNAMIC_PREFIX);
        let r = gd_ref(&name, EdgeKind::Calls);
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_bare_name_no_dot_no_autoload_is_none() {
        // A bare (no-dot) receiver name that is not a known autoload singleton
        // → the split yields (name, None), and with no singleton it is None.
        let ctx = Ctx::default();
        let r = gd_ref("SomeName", EdgeKind::Calls);
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_empty_receiver_is_none() {
        // A reference name beginning with `.` splits to an EMPTY receiver, which
        // the guard rejects with None (no fabricated resolution).
        let ctx = Ctx::default();
        let r = gd_ref(".member", EdgeKind::Calls);
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_class_member_resolves_to_function_in_class_file() {
        // A `Class.member()` Calls ref where `Class` is a real GDScript Class
        // node resolves to the uniquely-named Function in that class's file.
        let ctx = Ctx::default()
            .node(gd_node(
                "class:Weapon",
                NodeKind::Class,
                "Weapon",
                "weapon.gd",
                Language::Gdscript,
            ))
            .node(gd_node(
                "function:fire",
                NodeKind::Function,
                "fire",
                "weapon.gd",
                Language::Gdscript,
            ));
        let r = gd_ref("Weapon.fire", EdgeKind::Calls);
        let resolved = GodotResolver
            .resolve(&r, &ctx)
            .expect("class member must resolve");
        assert_eq!(resolved.target_node_id, "function:fire");
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
    }

    #[test]
    fn resolve_class_member_ambiguous_two_functions_is_none() {
        // Two file-level funcs named `fire` in the class file → ambiguous →
        // resolve_class_member returns None (never guesses).
        let ctx = Ctx::default()
            .node(gd_node(
                "class:Weapon",
                NodeKind::Class,
                "Weapon",
                "weapon.gd",
                Language::Gdscript,
            ))
            .node(gd_node(
                "function:fire1",
                NodeKind::Function,
                "fire",
                "weapon.gd",
                Language::Gdscript,
            ))
            .node(gd_node(
                "function:fire2",
                NodeKind::Function,
                "fire",
                "weapon.gd",
                Language::Gdscript,
            ));
        let r = gd_ref("Weapon.fire", EdgeKind::Calls);
        assert!(
            GodotResolver.resolve(&r, &ctx).is_none(),
            "ambiguous class member must not resolve"
        );
    }

    #[test]
    fn resolve_class_member_no_class_node_is_none() {
        // Receiver names no Class node → resolve_class_member None; and with no
        // autoload singleton either, resolve is None.
        let ctx = Ctx::default().node(gd_node(
            "function:fire",
            NodeKind::Function,
            "fire",
            "weapon.gd",
            Language::Gdscript,
        ));
        let r = gd_ref("Weapon.fire", EdgeKind::Calls);
        assert!(GodotResolver.resolve(&r, &ctx).is_none());
    }

    #[test]
    fn resolve_autoload_func_empty_receiver_or_member_is_none() {
        // `resolve_autoload_func` rejects an access with an empty receiver or an
        // empty member before touching the bindings.
        let ctx = Ctx::default().file("project.godot", &project_with_gameflow());
        assert!(resolve_autoload_func(&ctx, ".member").is_none());
        assert!(resolve_autoload_func(&ctx, "GameFlow.").is_none());
        assert!(resolve_autoload_func(&ctx, "no_dot").is_none());
    }

    #[test]
    fn resolve_autoload_func_unique_match_resolves() {
        // Exactly one same-named func in the bound script → resolves to it.
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .node(gd_node(
                "function:rtm",
                NodeKind::Function,
                "return_to_map",
                "game_flow.gd",
                Language::Gdscript,
            ));
        let target = resolve_autoload_func(&ctx, "GameFlow.return_to_map")
            .expect("unique func must resolve");
        assert_eq!(target.id, "function:rtm");
    }

    #[test]
    fn resolve_autoload_func_two_matches_is_none() {
        // Two same-named funcs → ambiguous → None.
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .node(gd_node(
                "function:rtm1",
                NodeKind::Function,
                "return_to_map",
                "game_flow.gd",
                Language::Gdscript,
            ))
            .node(gd_node(
                "function:rtm2",
                NodeKind::Function,
                "return_to_map",
                "game_flow.gd",
                Language::Gdscript,
            ));
        assert!(resolve_autoload_func(&ctx, "GameFlow.return_to_map").is_none());
    }

    #[test]
    fn autoload_script_bindings_skips_unreadable_project_file() {
        // A `project.godot` listed by get_all_files but whose content is
        // unreadable (read_file → None) is skipped without panic; the map stays
        // empty. `get_all_files` reports the path; `read_file` does not hold it.
        struct UnreadableCtx;
        impl ResolutionContext for UnreadableCtx {
            fn get_nodes_in_file(&self, _f: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_name(&self, _n: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_qualified_name(&self, _q: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_kind(&self, _k: NodeKind) -> Vec<Node> {
                Vec::new()
            }
            fn file_exists(&self, _f: &str) -> bool {
                false
            }
            fn read_file(&self, _f: &str) -> Option<String> {
                None
            }
            fn get_project_root(&self) -> &str {
                "/project"
            }
            fn get_all_files(&self) -> Vec<String> {
                vec!["project.godot".to_string()]
            }
            fn get_nodes_by_lower_name(&self, _l: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_node_by_id(&self, _id: &str) -> Option<Node> {
                None
            }
            fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
                Vec::new()
            }
        }
        let bindings = autoload_script_bindings(&UnreadableCtx);
        assert!(
            bindings.is_empty(),
            "an unreadable project.godot yields no bindings"
        );
    }

    #[test]
    fn autoload_script_bindings_dedups_repeated_project_file() {
        // A context whose get_all_files() reports the SAME project.godot twice
        // must be visited once (the seen_files dedup `continue`); the binding is
        // recorded exactly once, not duplicated.
        struct DupCtx;
        impl ResolutionContext for DupCtx {
            fn get_nodes_in_file(&self, _f: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_name(&self, _n: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_qualified_name(&self, _q: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_kind(&self, _k: NodeKind) -> Vec<Node> {
                Vec::new()
            }
            fn file_exists(&self, _f: &str) -> bool {
                true
            }
            fn read_file(&self, _f: &str) -> Option<String> {
                Some("[autoload]\n\nGameFlow=\"*res://game_flow.gd\"\n".to_string())
            }
            fn get_project_root(&self) -> &str {
                "/project"
            }
            fn get_all_files(&self) -> Vec<String> {
                vec!["project.godot".to_string(), "project.godot".to_string()]
            }
            fn get_nodes_by_lower_name(&self, _l: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_node_by_id(&self, _id: &str) -> Option<Node> {
                None
            }
            fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
                Vec::new()
            }
        }
        let bindings = autoload_script_bindings(&DupCtx);
        assert_eq!(
            bindings.len(),
            1,
            "the repeated project file is visited once"
        );
        assert_eq!(
            bindings.get("GameFlow").map(String::as_str),
            Some("game_flow.gd")
        );
    }

    #[test]
    fn resolve_class_member_none_falls_through_to_singleton_path() {
        // A `Class.member` Calls ref where resolve_class_member returns None
        // (no Class node) must FALL THROUGH the class-member block and reach the
        // autoload-singleton lookup — which, with a matching singleton, resolves
        // to the singleton node (exercising the post-class-member fall-through).
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .node(gd_node(
                "const:GameFlow",
                NodeKind::Constant,
                "GameFlow",
                "project.godot",
                Language::GodotProject,
            ));
        let r = gd_ref("GameFlow.some_method", EdgeKind::Calls);
        let resolved = GodotResolver
            .resolve(&r, &ctx)
            .expect("falls through to singleton");
        assert_eq!(resolved.target_node_id, "const:GameFlow");
    }

    #[test]
    fn post_extract_skips_singleton_with_no_binding() {
        // A `project.godot` Constant singleton whose name has NO `[autoload]`
        // script binding is left untouched (the `continue` at the missing-binding
        // guard). Here the file binds `GameFlow` but the singleton is `Orphan`.
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .node(gd_node(
                "const:Orphan",
                NodeKind::Constant,
                "Orphan",
                "project.godot",
                Language::GodotProject,
            ));
        let updates = GodotResolver.post_extract(&ctx).expect("post_extract Some");
        assert!(
            updates.is_empty(),
            "a singleton with no binding is not stamped, got {updates:?}"
        );
    }

    #[test]
    fn post_extract_is_idempotent_when_already_stamped() {
        // A singleton already carrying the confirmed signature is skipped (the
        // idempotency guard), so a second pass produces zero updates.
        let mut singleton = gd_node(
            "const:GameFlow",
            NodeKind::Constant,
            "GameFlow",
            "project.godot",
            Language::GodotProject,
        );
        singleton.signature = Some("autoload -> game_flow.gd".to_string());
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .file("game_flow.gd", "extends Node\n")
            .node(singleton);
        let updates = GodotResolver.post_extract(&ctx).expect("post_extract Some");
        assert!(
            updates.is_empty(),
            "an already-stamped singleton is not re-stamped, got {updates:?}"
        );
    }

    #[test]
    fn post_extract_stamps_confirmed_binding() {
        // Control: an unstamped singleton whose bound script exists gets the
        // confirmed signature stamped.
        let ctx = Ctx::default()
            .file("project.godot", &project_with_gameflow())
            .file("game_flow.gd", "extends Node\n")
            .node(gd_node(
                "const:GameFlow",
                NodeKind::Constant,
                "GameFlow",
                "project.godot",
                Language::GodotProject,
            ));
        let updates = GodotResolver.post_extract(&ctx).expect("post_extract Some");
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].signature.as_deref(),
            Some("autoload -> game_flow.gd")
        );
    }
}
