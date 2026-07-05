//! `.tres` resource parser — L4 of Godot static analysis (T5).
//!
//! Parses a Godot `.tres` text resource into the script/resource references it
//! holds. [`crate::frameworks::godot::GodotResolver::extract`] dispatches here
//! when a file's basename ends in `.tres`. Cross-file resolution of the emitted
//! reference paths to their target script/scene/resource nodes is T7's
//! `post_extract` job; this layer only parses + emits.
//!
//! # Format (the subset L4 reads)
//!
//! A `.tres` is the same flat resource grammar as `project.godot` / `.tscn`:
//! `[section]` headers followed by `key = value` lines. Unlike a `.tscn` it has
//! no scene tree — it is ONE resource, so L4 is a simpler cousin of the L2
//! (`.tscn`) parser: the same `[ext_resource]` id-table + `ExtResource("id")`
//! mechanics, minus the node tree and signal connections. L4 reads:
//!
//! - `[gd_resource type="..." ...]` — the resource's own header. Its `type`
//!   names the resource marker node (so the file's edges have a stable,
//!   human-meaningful source name). Header-only — declares no reference.
//! - `[ext_resource type="..." path="res://..." id="..."]` — an external
//!   resource declaration. Each builds an in-file `id → repo-relative path`
//!   lookup entry (IDENTICAL to T4); an ext_resource is NOT itself a graph node.
//! - `[resource]` — the resource body. Every `key = ExtResource("id")` line
//!   under it resolves the id through the ext_resource table and emits a
//!   reference to that repo-relative path:
//!   - `script = ExtResource("id")` → a resource→script reference (the script
//!     the resource is an instance of — e.g. a `Buff` resource backed by
//!     `buff.gd`).
//!   - any OTHER `prop = ExtResource("id")` → a resource→resource reference
//!     (e.g. a `Buff` whose `effect` property points at another `.tres`/scene/
//!     script). Both reuse [`EdgeKind::References`]; L4 does not distinguish them
//!     at the edge level (the target path's extension carries the kind, and T7
//!     resolves it).
//!
//! # Anchor-node choice (the one structural decision vs T4)
//!
//! A `.tscn` has many scene nodes, so T4 anchors each reference on the scene
//! node that declared it. A `.tres` is a SINGLE flat resource — there is no
//! per-node anchor. L4 therefore emits ONE resource marker node
//! ([`NodeKind::Constant`], consistent with T3/T4's reuse — no new variant per
//! the golden blast-radius constraint), named after the `[gd_resource]` header
//! `type` (falling back to `"resource"`), and anchors EVERY reference edge on
//! it. The T1 `file:{relpath}` node already exists for the file itself; the
//! marker is the in-resource source the reference edges hang from, the analogue
//! of T4's scene node. The marker is emitted LAZILY — only when at least one
//! reference will be anchored — so a self-contained resource with no
//! ext_resource yields the file node ONLY and ZERO extra nodes/edges (no
//! spurious output, mirroring T3's lazy `editor_plugins` marker).
//!
//! # res:// mapping
//!
//! `ext_resource path="res://..."` maps to a repo-relative path with the SAME
//! shared rule L1/L2 use ([`super::godot_common::map_res_path`]): strip the
//! surrounding quotes, a leading `*`, the `res://` scheme, and any remaining
//! leading `/`.
//!
//! # Tolerance
//!
//! Every section and line is parsed defensively: a malformed `[section ...]`
//! header (no closing `]`), a line with no `=`, an unterminated quote, or an
//! `ExtResource("id")` whose id has no matching declaration is skipped, never
//! panics. An empty file yields an empty result. Binary `.res` is NOT parsed
//! (only the text `.tres`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, ReferenceSubkind};

use super::framework_node;
use super::godot_common::{map_res_path, quoted_strings, strip_quotes};
use super::godot_dsl_config::{IdFieldSpec, dsl_id_fields, dsl_resource_fields};
use crate::types::{FrameworkResolverExtractionResult, RefView};

/// The marker-node name used when a `.tres` has no `[gd_resource type="..."]`.
const DEFAULT_RESOURCE_NAME: &str = "resource";

/// `true` when `file_path`'s basename ends in `.tres` (case-sensitive, matching
/// Godot's own extension). Matches by extension the same defensive way L2
/// matches `.tscn`, so nested paths dispatch too. Binary `.res` is excluded.
pub(crate) fn is_tres(file_path: &str) -> bool {
    file_path
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base.ends_with(".tres"))
}

/// The path the DSL config reader walks up from. An absolute `file_path` is used
/// as-is (test callers pass absolute paths; `find_config_path` walks from its
/// parent). A relative `file_path` (the pipeline's repo-relative path) is joined
/// onto `project_root` so the walk starts at the file's real on-disk location —
/// independent of the process CWD. An empty `project_root` falls back to the
/// `file_path` verbatim (the pre-fix behavior, for callers that supply none).
fn config_lookup_path(file_path: &str, project_root: &str) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() || project_root.is_empty() {
        p.to_path_buf()
    } else {
        Path::new(project_root).join(file_path)
    }
}

/// Parse a `.tres` resource into a resource marker node + its script/resource
/// references.
///
/// Deterministic: the (single) marker node and its references are emitted in
/// source order; the node id follows the upstream
/// `{kind}:{sha256("{filePath}:{kind}:{name}:{line}").hex[:32]}` formula via
/// [`generate_node_id`]. Lines are 1-based.
pub(crate) fn parse_tres(
    file_path: &str,
    content: &str,
    project_root: &str,
) -> FrameworkResolverExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<RefView> = Vec::new();

    // The DSL config is resolved against the project root, NOT `file_path`'s
    // CWD-relative join: the pipeline passes a repo-RELATIVE `file_path` (used
    // verbatim for golden-stable node/ref attribution below), so config lookup
    // walks up from the file's ABSOLUTE location (`project_root` + `file_path`).
    // A `file_path` that is already absolute is used as-is. This keeps
    // attribution relative while letting `find_config_path` find the project's
    // `.codegraph/codegraph.json` regardless of the process CWD.
    let config_lookup_path = config_lookup_path(file_path, project_root);

    // OPT-IN DSL fields (T9): the `godot.dsl.resourceFields` list from the
    // nearest `.codegraph/codegraph.json` (empty when absent — the off-by-default
    // case, so zero DSL behavior). A `key = value` line whose key is in this list
    // emits a reference to its value (string literal → the literal text).
    let dsl_fields = dsl_resource_fields(&config_lookup_path);

    // OPT-IN ID fields (PR2): the `godot.dsl.idFields` spec map from the same
    // nearest config (empty when absent — off-by-default). A `key = value` line
    // whose key is a spec key emits one `godot:id:<kind>:<value>` sentinel per
    // selected segment. Independent of `dsl_fields`: a line may match both.
    let id_fields = dsl_id_fields(&config_lookup_path);

    // In-file ext_resource lookup: id → repo-relative path. Built top-down as
    // the file is scanned (Godot declares ext_resources before `[resource]`),
    // so a `key = ExtResource("id")` line resolves inline (same as T4).
    let mut ext_resources: HashMap<String, String> = HashMap::new();

    // The resource's own type, from `[gd_resource type="..."]`, used to name the
    // marker node. Defaults if absent or unnamed.
    let mut resource_type = DEFAULT_RESOURCE_NAME.to_string();
    // The marker node, created lazily on the first reference to anchor. Once
    // created it is reused for every subsequent reference.
    let mut marker: Option<MarkerNode> = None;
    // `true` once we are inside the `[resource]` body (where `ExtResource`
    // property bindings live). `key = value` lines outside it are ignored.
    let mut in_resource = false;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = (idx + 1) as i64;
        let line = raw_line.trim();

        if line.is_empty() || is_comment(line) {
            continue;
        }

        if let Some(header) = parse_section_header(line) {
            in_resource = false;
            match header.name {
                "gd_resource" => {
                    if let Some(ty) = attr(&header.attrs, "type") {
                        if !ty.is_empty() {
                            resource_type = ty.to_string();
                        }
                    }
                }
                "ext_resource" => record_ext_resource(&header.attrs, &mut ext_resources),
                "resource" => in_resource = true,
                _ => {} // sub_resource / unknown → ignored
            }
            continue;
        }

        // A `key = value` line: only meaningful inside the `[resource]` body.
        if !in_resource {
            continue;
        }
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        // The standard T5 edge: a value resolving as `ExtResource("id")` emits a
        // reference to the resolved repo-relative path, for ANY key. Non-handle
        // values fall through to the opt-in DSL check.
        let target = match resolve_ext_resource(value, &ext_resources) {
            Some(resolved) => Some(resolved),
            None => dsl_literal_target(key, value, &dsl_fields),
        };
        if let Some(target) = target {
            let from = marker_id(&mut marker, &mut nodes, file_path, line_no, &resource_type);
            references.push(reference(from, target, line_no, file_path));
        }
        // The opt-in PR2 ID edges: one `godot:id:<kind>:<value>` sentinel per
        // configured segment, emitted AFTER the standard edge so a line that
        // matches both keeps source-line order. Empty when `id_fields` is empty.
        for sentinel in dsl_id_targets(key, value, &id_fields) {
            let from = marker_id(&mut marker, &mut nodes, file_path, line_no, &resource_type);
            references.push(reference(from, sentinel, line_no, file_path));
        }
    }

    FrameworkResolverExtractionResult { nodes, references }
}

/// Lazily create (once) and return the resource marker node id, pushing the
/// marker node into `nodes` on first use. Shared by the standard T5 edge and the
/// PR2 ID-sentinel edges so the marker is created exactly once, on the first
/// reference to anchor.
fn marker_id(
    marker: &mut Option<MarkerNode>,
    nodes: &mut Vec<Node>,
    file_path: &str,
    line_no: i64,
    resource_type: &str,
) -> String {
    marker
        .get_or_insert_with(|| {
            let node = constant_node(file_path, line_no, resource_type);
            let id = node.id.clone();
            nodes.push(node);
            MarkerNode { id }
        })
        .id
        .clone()
}

/// The single resource marker node, once created.
struct MarkerNode {
    id: String,
}

/// Record an `[ext_resource type="..." path="res://..." id="..."]` declaration
/// into the id→path lookup. Skipped if it has no id or its path is not a
/// resolvable `res://` path. IDENTICAL to T4's `record_ext_resource`.
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

/// The OPT-IN DSL edge target (T9). Returns the string-literal value of `key`
/// ONLY when `key` is one of the configured `dsl_fields` and `value` is a plain
/// double-quoted string. With no config `dsl_fields` is empty, so this is always
/// `None` — the off-by-default contract. A non-empty quoted value yields its
/// inner text as the reference target.
fn dsl_literal_target(key: &str, value: &str, dsl_fields: &[String]) -> Option<String> {
    if !dsl_fields.iter().any(|f| f == key) {
        return None;
    }
    let trimmed = value.trim();
    if !(trimmed.starts_with('"') && trimmed.ends_with('"')) {
        return None;
    }
    let inner = strip_quotes(trimmed);
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// The OPT-IN ID-field sentinels (PR2). Returns one `godot:id:<kind>:<value>`
/// sentinel string per ID captured from `key = value` when `key` is a configured
/// `id_fields` spec. With no config `id_fields` is empty, so this is always an
/// empty `Vec` — the off-by-default contract.
///
/// The value is quote-stripped first. A spec WITHOUT a `separator` (or with an
/// empty one) yields ONE sentinel for the whole stripped value. A spec WITH a
/// `separator` splits the stripped value and selects the `id_segments` (0-based)
/// parts; an out-of-range index is silently skipped (tolerant). A spec with
/// `id_segments` but no `separator` treats the whole value as one ID (segments
/// inert), per D-A3. Empty captured segments are dropped so a sentinel always
/// carries a non-empty value.
fn dsl_id_targets(
    key: &str,
    value: &str,
    id_fields: &BTreeMap<String, IdFieldSpec>,
) -> Vec<String> {
    let Some(spec) = id_fields.get(key) else {
        return Vec::new();
    };
    let stripped = strip_quotes(value.trim());

    let values: Vec<&str> = match spec.separator.as_deref() {
        Some(sep) if !sep.is_empty() => {
            let parts: Vec<&str> = stripped.split(sep).collect();
            match &spec.id_segments {
                Some(segments) => segments
                    .iter()
                    .filter_map(|&i| parts.get(i).copied())
                    .collect(),
                None => parts,
            }
        }
        _ => vec![stripped],
    };

    values
        .into_iter()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("godot:id:{}:{}", spec.kind, v))
        .collect()
}

/// Build a [`NodeKind::Constant`] resource marker node with the deterministic
/// upstream id.
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
        Language::GodotResource,
        false,
    )
}

/// Build a resource→target `References` edge from the marker node to a
/// repo-relative path.
fn reference(from_node_id: String, target: String, line_no: i64, file_path: &str) -> RefView {
    RefView {
        from_node_id,
        reference_name: target,
        reference_kind: EdgeKind::References,
        line: line_no,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::GodotResource,
        is_function_ref: false,
        reference_subkind: Some(ReferenceSubkind::ExtResource),
    }
}

// ---------------------------------------------------------------------------
// Line-level parsing helpers (shape-shared with godot_scene.rs; the genuinely
// shared res://-mapping + quote/array helpers live in godot_common.rs)
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
/// Values may be double-quoted (`type="Resource"`) or bare expressions; a
/// balanced-paren-aware scan keeps `ExtResource("2")`-style tokens whole.
/// Same scanner shape as T4 (the `.tres` header attribute set is a subset of
/// `.tscn`'s, so the tokenizer is identical).
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
/// rather than duplicating the unquoted variant. Same helper shape as T4.
fn quote(unquoted: &str) -> String {
    let bare = strip_quotes(unquoted);
    format!("\"{bare}\"")
}
