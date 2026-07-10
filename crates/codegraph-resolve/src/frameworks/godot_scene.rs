//! `.tscn` scene parser — L2 of Godot static analysis (T4).
//!
//! Parses a Godot `.tscn` scene file into a node-tree plus the edges that bind
//! scene nodes to the scripts, subscenes, signal handlers, and groups they
//! reference. [`crate::frameworks::godot::GodotResolver::extract`] dispatches
//! here when a file's basename ends in `.tscn`. Cross-file resolution of the
//! emitted reference names/paths to their target script/scene/method nodes is
//! T7's `post_extract` job; this layer only parses + emits.
//!
//! # Format (the subset L2 reads)
//!
//! A `.tscn` is the same flat resource grammar as `project.godot`: `[section]`
//! headers followed by `key = value` lines. L2 reads:
//!
//! - `[ext_resource type="..." path="res://..." id="..."]` — an external
//!   resource declaration. Each builds an in-file `id → (type, path)` lookup
//!   entry; an ext_resource is NOT itself a graph node. The id is later
//!   referenced by `ExtResource("id")` handles (in `script =` / `instance=`).
//! - `[node name="N" type="T" parent="P"]` — a scene-tree node. Emits one
//!   [`NodeKind::Constant`] node per scene node (the parent path describes the
//!   tree structure; it is not itself emitted as an edge here).
//!   - `script = ExtResource("id")` under the node — a scene→script binding
//!     ([`EdgeKind::References`]) from the node to the resolved script path.
//!   - `groups = ["g1", "g2"]` under the node — one group-membership reference
//!     ([`EdgeKind::References`]) from the node to each group NAME.
//!   - `instance=ExtResource("id")` on the node header — an instanced-subscene
//!     edge ([`EdgeKind::Instantiates`]) from the node to the resolved `.tscn`
//!     path.
//! - `[connection signal="s" from="A" to="B" method="m"]` — a signal wiring.
//!   Emits a reference ([`EdgeKind::References`]) to the handler method NAME
//!   `m` — the "who handles this signal" edge. The reference originates from the
//!   `from` scene node when that node is known, else from a synthesized
//!   connection marker; T7 resolves `m` to the actual function symbol.
//!
//! `[sub_resource]` and `.tres`-style resource sections are intentionally NOT
//! parsed here (`.tres` is T5).
//!
//! # NodeKind / EdgeKind choices
//!
//! Scene nodes reuse [`NodeKind::Constant`] — consistent with L1 (T3), which
//! used `Constant` for autoload singletons / input actions: a scene node is a
//! named structural element addressed by name within the scene tree, the
//! closest honest reuse of an existing kind (no new `NodeKind` variant, per the
//! golden blast-radius constraint). Script-binding, signal-handler, and
//! group-membership edges reuse [`EdgeKind::References`] (as L1 did for all its
//! edges); an instanced subscene reuses [`EdgeKind::Instantiates`] — Godot
//! literally instances a `PackedScene`, so `Instantiates` is the honest reuse.
//!
//! # res:// mapping
//!
//! `ext_resource path="res://..."` maps to a repo-relative path with the SAME
//! rule L1 uses (shared [`super::godot_common::map_res_path`]): strip the
//! surrounding quotes, a leading `*`, the `res://` scheme, and any remaining
//! leading `/`.
//!
//! # Tolerance
//!
//! Every section and line is parsed defensively: a malformed `[section ...]`
//! header (no closing `]`), a line with no `=` inside a node, an unterminated
//! quote, or an `ExtResource("id")` whose id has no matching `ext_resource`
//! declaration is skipped, never panics. An empty file yields an empty result.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, ReferenceSubkind};

use super::framework_node;
use super::godot_common::{map_res_path, quoted_strings, strip_quotes};
use crate::types::{FrameworkResolverExtractionResult, RefView};

/// `true` when `file_path`'s basename ends in `.tscn` (case-sensitive, matching
/// Godot's own extension). Matches by extension the same defensive way L1
/// matches `project.godot` by basename, so nested paths dispatch too.
pub(crate) fn is_tscn(file_path: &str) -> bool {
    file_path
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base.ends_with(".tscn"))
}

/// Parse a `.tscn` scene into scene-tree nodes + their script/signal/group/
/// instance references.
///
/// Deterministic: nodes are emitted in source order; ids follow the upstream
/// `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}` formula via
/// [`generate_node_id`]. Lines are 1-based.
pub(crate) fn parse_tscn(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<RefView> = Vec::new();

    // In-file ext_resource lookup: id → repo-relative path. Built as the file is
    // scanned top-down (Godot declares ext_resources before the nodes that use
    // them), so a node's `ExtResource("id")` handle can be resolved inline.
    let mut ext_resources: HashMap<String, String> = HashMap::new();

    // The scene node the current line-block belongs to, for `script =` /
    // `groups =` props that follow a `[node ...]` header.
    let mut current_node: Option<CurrentNode> = None;
    // Map scene-node NAME → its node id, so a `[connection from="X"]` can
    // originate from node X when it is known.
    let mut node_ids: HashMap<String, String> = HashMap::new();
    // The scene's root node id — the first `[node]` (which has no `parent`).
    // Godot writes `from="."`/`to="."` to mean this root/self node, so a
    // connection anchored at "." resolves here instead of a phantom marker.
    let mut root_node_id: Option<String> = None;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as i64;
        let line = raw_line.trim();

        if line.is_empty() || is_comment(line) {
            continue;
        }

        if let Some(header) = parse_section_header(line) {
            // Leaving any previous node block.
            current_node = None;
            match header.name {
                "ext_resource" => record_ext_resource(&header.attrs, &mut ext_resources),
                "node" => {
                    if let Some(cn) = emit_node(
                        file_path,
                        line_no,
                        &header.attrs,
                        &ext_resources,
                        &mut nodes,
                        &mut references,
                    ) {
                        if root_node_id.is_none() && attr(&header.attrs, "parent").is_none() {
                            root_node_id = Some(cn.id.clone());
                        }
                        node_ids.insert(cn.name.clone(), cn.id.clone());
                        current_node = Some(cn);
                    }
                }
                "connection" => emit_connection(
                    file_path,
                    line_no,
                    &header.attrs,
                    &node_ids,
                    root_node_id.as_deref(),
                    &mut references,
                ),
                _ => {} // sub_resource / gd_scene / unknown → ignored
            }
            continue;
        }

        // A `key = value` line: only meaningful inside a `[node]` block.
        let Some(node) = &current_node else {
            continue;
        };
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        match key {
            "script" => emit_script(
                file_path,
                line_no,
                node,
                value,
                &ext_resources,
                &mut references,
            ),
            "groups" => emit_groups(file_path, line_no, node, value, &mut references),
            _ => {}
        }
    }

    FrameworkResolverExtractionResult { nodes, references }
}

/// Parse the scene's own UID from its `[gd_scene ... uid="uid://…"]` header
/// line. Godot stamps every saved `.tscn` with a stable `uid="uid://<id>"`
/// attribute on the opening `gd_scene` header; this is the id a
/// `project.godot` `run/main_scene="uid://<id>"` (and `[ext_resource
/// uid="uid://…"]` handles) point at.
///
/// Returns the FULL `uid://<id>` string (scheme kept, matching how it appears in
/// `run/main_scene`), or `None` when the file has no `gd_scene` header or no
/// `uid` attribute. Scans only the header — resource contents are not
/// interpreted (static-format parse only, in scope per the doc).
pub(crate) fn parse_scene_uid(content: &str) -> Option<String> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || is_comment(line) {
            continue;
        }
        let header = parse_section_header(line)?;
        if header.name != "gd_scene" {
            // The first non-blank header of a `.tscn` is always `[gd_scene …]`;
            // if the first header is anything else there is no scene uid.
            return None;
        }
        let uid = attr(&header.attrs, "uid")?.trim();
        if uid.is_empty() {
            return None;
        }
        return Some(uid.to_string());
    }
    None
}

/// Build a deterministic `uid://<id>` → repo-relative `.tscn` path map by
/// reading every `.tscn` under `project_root`.
///
/// Determinism: the discovered files are sorted lexicographically before
/// parsing, and the map is FIRST-WRITE-WINS on a duplicate uid (mirroring the
/// `[autoload]` dup rule in `godot_project::autoload_script_paths`), so the
/// result is independent of directory-iteration order. A file with no
/// `gd_scene`/`uid` header contributes nothing. An empty or unreadable
/// `project_root` yields an empty map.
pub(crate) fn scene_uid_map(project_root: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    if project_root.is_empty() {
        return out;
    }
    let root = Path::new(project_root);
    let mut tscn_files: Vec<PathBuf> = Vec::new();
    collect_tscn_files(root, &mut tscn_files);
    tscn_files.sort();
    for full in tscn_files {
        let Ok(rel) = full.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Ok(content) = std::fs::read_to_string(&full) else {
            continue;
        };
        if let Some(uid) = parse_scene_uid(&content) {
            out.entry(uid).or_insert(rel);
        }
    }
    out
}

/// Recursively collect `.tscn` files under `dir`, skipping the engine-managed
/// `.godot/` cache and any hidden dir (`.git`, etc.). Errors are ignored
/// (tolerant walk); ordering is imposed by the caller's sort.
fn collect_tscn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if !skip {
                collect_tscn_files(&path, out);
            }
        } else if is_tscn(&path.to_string_lossy()) {
            out.push(path);
        }
    }
}

/// The scene node a `key = value` block currently belongs to.
struct CurrentNode {
    id: String,
    name: String,
}

/// Record an `[ext_resource type="..." path="res://..." id="..."]` declaration
/// into the id→path lookup. Skipped if it has no id or its path is not a
/// resolvable `res://` path.
fn record_ext_resource(attrs: &[(String, String)], ext_resources: &mut HashMap<String, String>) {
    let Some(id) = attr(attrs, "id") else {
        return;
    };
    let Some(path_attr) = attr(attrs, "path") else {
        return;
    };
    if let Some(rel) = map_res_path(&quote(path_attr)) {
        ext_resources.insert(id.to_string(), rel);
    }
}

/// Emit a scene-tree node ([`NodeKind::Constant`]) for `[node name="N" ...]`,
/// plus an [`EdgeKind::Instantiates`] reference if the header carries
/// `instance=ExtResource("id")`. Returns the [`CurrentNode`] so following
/// `script`/`groups` lines bind to it; `None` if the node has no name.
fn emit_node(
    file_path: &str,
    line_no: i64,
    attrs: &[(String, String)],
    ext_resources: &HashMap<String, String>,
    nodes: &mut Vec<Node>,
    references: &mut Vec<RefView>,
) -> Option<CurrentNode> {
    let name = attr(attrs, "name")?;
    if name.is_empty() {
        return None;
    }
    let node = constant_node(file_path, line_no, name);
    let node_id = node.id.clone();
    nodes.push(node);

    // `instance=ExtResource("id")` on the header → instanced-subscene edge.
    if let Some(instance) = attr(attrs, "instance") {
        if let Some(target) = resolve_ext_resource(instance, ext_resources) {
            references.push(reference(
                node_id.clone(),
                target,
                EdgeKind::Instantiates,
                line_no,
                file_path,
                ReferenceSubkind::SceneInstance,
            ));
        }
    }

    Some(CurrentNode {
        id: node_id,
        name: name.to_string(),
    })
}

/// Emit a scene→script `References` edge from `node` to the script path the
/// `script = ExtResource("id")` value resolves to. Skipped if the id is unknown.
fn emit_script(
    file_path: &str,
    line_no: i64,
    node: &CurrentNode,
    value: &str,
    ext_resources: &HashMap<String, String>,
    references: &mut Vec<RefView>,
) {
    if let Some(target) = resolve_ext_resource(value, ext_resources) {
        references.push(reference(
            node.id.clone(),
            target,
            EdgeKind::References,
            line_no,
            file_path,
            ReferenceSubkind::ScriptAttach,
        ));
    }
}

/// Emit one group-membership `References` edge from `node` to each group NAME in
/// a `groups = ["g1", "g2"]` value.
fn emit_groups(
    file_path: &str,
    line_no: i64,
    node: &CurrentNode,
    value: &str,
    references: &mut Vec<RefView>,
) {
    for group in quoted_strings(value) {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        references.push(reference(
            node.id.clone(),
            group.to_string(),
            EdgeKind::References,
            line_no,
            file_path,
            ReferenceSubkind::GroupMember,
        ));
    }
}

/// Emit a signal-connection `References` edge to the handler method NAME from a
/// `[connection signal="s" from="A" to="B" method="m"]`. The edge originates
/// from the `from` scene node when that node is known; from the scene root node
/// when `from="."` (Godot's self/root marker); else from a synthesized
/// connection marker node (so the handler name is still recorded). T7 resolves
/// the method name to its actual symbol.
///
/// Anchoring `from="."` to the real, emitted root node (rather than a marker id
/// that is never pushed as a `Node`) is what keeps the ref alive: the store's
/// `insert_unresolved_refs` drops any ref whose `from_node_id` is absent from
/// `nodes`, so a phantom-marker anchor would silently vanish during persistence.
fn emit_connection(
    file_path: &str,
    line_no: i64,
    attrs: &[(String, String)],
    node_ids: &HashMap<String, String>,
    root_node_id: Option<&str>,
    references: &mut Vec<RefView>,
) {
    let Some(method) = attr(attrs, "method") else {
        return;
    };
    if method.is_empty() {
        return;
    }
    let from = attr(attrs, "from").unwrap_or("");
    let from_node_id = node_ids
        .get(from)
        .cloned()
        // Godot's self/root marker — anchor to the actual scene root node.
        .or_else(|| {
            if from == "." {
                root_node_id.map(str::to_string)
            } else {
                None
            }
        })
        // No matching scene node: cannot happen for a well-formed scene, but be
        // tolerant — anchor the handler ref to a deterministic connection marker
        // keyed on the signal/from/to so the method name is not lost.
        .unwrap_or_else(|| {
            let signal = attr(attrs, "signal").unwrap_or("");
            let to = attr(attrs, "to").unwrap_or("");
            let marker = format!("connection:{from}:{signal}:{to}");
            generate_node_id(file_path, NodeKind::Constant, &marker, line_no as u32)
        });
    references.push(reference(
        from_node_id,
        method.to_string(),
        EdgeKind::References,
        line_no,
        file_path,
        ReferenceSubkind::SignalMethod,
    ));
}

/// Resolve an `ExtResource("id")` handle (possibly with surrounding whitespace)
/// to its repo-relative path via the in-file ext_resource lookup. Returns `None`
/// if the value is not an `ExtResource(...)` handle or the id is unknown.
fn resolve_ext_resource(value: &str, ext_resources: &HashMap<String, String>) -> Option<String> {
    let id = parse_ext_resource_id(value)?;
    ext_resources.get(id).cloned()
}

/// Pull the quoted id out of `ExtResource("id")`. Returns `None` if `value` is
/// not such a handle.
fn parse_ext_resource_id(value: &str) -> Option<&str> {
    let inner = value.trim().strip_prefix("ExtResource")?;
    let inner = inner.trim_start();
    let inner = inner.strip_prefix('(')?.strip_suffix(')')?;
    quoted_strings(inner).into_iter().next()
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
        Language::GodotScene,
        false,
    )
}

/// Build an edge from a scene node to a repo-relative path or NAME.
fn reference(
    from_node_id: String,
    target: String,
    kind: EdgeKind,
    line_no: i64,
    file_path: &str,
    subkind: ReferenceSubkind,
) -> RefView {
    RefView {
        from_node_id,
        reference_name: target,
        reference_kind: kind,
        line: line_no,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::GodotScene,
        is_function_ref: false,
        reference_subkind: Some(subkind),
    }
}

// ---------------------------------------------------------------------------
// Line-level parsing helpers
// ---------------------------------------------------------------------------

fn is_comment(line: &str) -> bool {
    line.starts_with(';') || line.starts_with('#')
}

/// A parsed `[name attr="v" attr2=Expr(...)]` section header.
struct SectionHeader<'a> {
    name: &'a str,
    attrs: Vec<(String, String)>,
}

/// `[name a="b" c=Expr("d")]` → `Some(SectionHeader)`. Requires the trimmed line
/// to start `[` and end `]`. Returns `None` (skipped) for any line that is not a
/// well-formed `[...]` header.
fn parse_section_header(line: &str) -> Option<SectionHeader<'_>> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    let mut it = inner.splitn(2, char::is_whitespace);
    let name = it.next()?.trim();
    if name.is_empty() {
        return None;
    }
    let attrs = it.next().map(parse_attrs).unwrap_or_default();
    Some(SectionHeader { name, attrs })
}

/// Parse the attribute tail of a section header into `(key, value)` pairs.
/// Values may be double-quoted (`name="Player"`) or bare expressions
/// (`instance=ExtResource("2")`); the raw value text (without surrounding
/// quotes when quoted) is captured and the consumers re-interpret it.
///
/// A balanced-paren-aware scan keeps `ExtResource("2")` (which contains a space-
/// free but paren-wrapped, quote-bearing value) as a single token.
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let bytes = s.as_bytes();
    let mut attrs = Vec::new();
    let mut i = 0usize;
    let len = bytes.len();
    while i < len {
        // Skip leading whitespace.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Read key up to `=`.
        let key_start = i;
        while i < len && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = s[key_start..i].trim();
        // Skip whitespace before `=`.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || bytes[i] != b'=' {
            // A bare token with no `=`: skip it, stay tolerant.
            continue;
        }
        i += 1; // consume `=`
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        let value = if bytes[i] == b'"' {
            // Quoted value: capture inner text, advance past the closing quote.
            let start = i + 1;
            let mut j = start;
            while j < len && bytes[j] != b'"' {
                j += 1;
            }
            let v = &s[start..j.min(len)];
            i = if j < len { j + 1 } else { len };
            v.to_string()
        } else {
            // Bare value: read to the next top-level whitespace, honoring
            // parens so `ExtResource("2")` stays whole.
            let start = i;
            let mut depth = 0i32;
            while i < len {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    b if b.is_ascii_whitespace() && depth <= 0 => break,
                    _ => {}
                }
                i += 1;
            }
            s[start..i].trim().to_string()
        };
        if !key.is_empty() {
            attrs.push((key.to_string(), value));
        }
    }
    attrs
}

/// Look up an attribute value by key (first match).
fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Split `key = value` at the FIRST `=`. Returns `None` when there is no `=`.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    let value = line[eq + 1..].trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Re-wrap an already-unquoted token in double quotes so it can be fed to the
/// shared [`map_res_path`] (which expects a quoted Godot value). The attribute
/// parser strips quotes from `path="res://..."`, but `map_res_path`'s contract
/// is "quoted value in"; this keeps the shared rule the single source of truth
/// rather than duplicating the unquoted variant.
fn quote(unquoted: &str) -> String {
    // strip_quotes is idempotent on an already-unquoted token, so wrapping then
    // letting map_res_path strip is safe; this also tolerates a value that
    // somehow still carries quotes.
    let bare = strip_quotes(unquoted);
    format!("\"{bare}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scene_uid_reads_gd_scene_header() {
        let content = "[gd_scene load_steps=2 format=3 uid=\"uid://dr6r06q24gfti\"]\n\n[node name=\"Root\" type=\"Node\"]\n";
        assert_eq!(
            parse_scene_uid(content).as_deref(),
            Some("uid://dr6r06q24gfti")
        );
    }

    #[test]
    fn parse_scene_uid_none_without_uid_attr() {
        let content = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node\"]\n";
        assert!(parse_scene_uid(content).is_none());
    }

    #[test]
    fn parse_scene_uid_none_when_first_header_is_not_gd_scene() {
        let content = "[node name=\"Root\" type=\"Node\"]\n";
        assert!(parse_scene_uid(content).is_none());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-sceneuid-{tag}-{}-{}",
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
    fn scene_uid_map_maps_uid_to_relative_path() {
        let dir = temp_dir("basic");
        std::fs::create_dir_all(dir.join("Scenes")).unwrap();
        std::fs::write(
            dir.join("Scenes/menu.tscn"),
            "[gd_scene format=3 uid=\"uid://xyz\"]\n",
        )
        .unwrap();
        let map = scene_uid_map(&dir.to_string_lossy());
        assert_eq!(
            map.get("uid://xyz").map(String::as_str),
            Some("Scenes/menu.tscn")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scene_uid_map_first_write_wins_on_dup_uid() {
        let dir = temp_dir("dup");
        std::fs::write(
            dir.join("a.tscn"),
            "[gd_scene format=3 uid=\"uid://same\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.tscn"),
            "[gd_scene format=3 uid=\"uid://same\"]\n",
        )
        .unwrap();
        let map = scene_uid_map(&dir.to_string_lossy());
        assert_eq!(
            map.get("uid://same").map(String::as_str),
            Some("a.tscn"),
            "sorted walk + first-write-wins picks the lexicographically first path"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scene_uid_map_empty_root_is_empty() {
        assert!(scene_uid_map("").is_empty());
    }
}
