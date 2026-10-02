//! Where the static path ends at runtime dispatch — the structured half of
//! upstream `src/graph/dynamic-boundary-report.ts` (`findDynamicBoundaries`,
//! `shortlistBoundaryCandidates`, `v1.6.1`) that the Flow strip's end cap
//! renders.
//!
//! The scan is `codegraph_explore`'s own dispatch scanner
//! (`codegraph_mcp::dynamic_boundaries`), so the form, the key and the candidate
//! targets are the ones explore would print. Bodies are read through the
//! viewer's chokepoint, and a file that drifted since the index is skipped: its
//! recorded line ranges no longer name the body.

use std::collections::HashSet;

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use codegraph_graph::named_symbol_flow::OrderedMap;
use codegraph_graph::query::{SearchOptions, search_nodes};
use codegraph_mcp::dynamic_boundaries::{BoundaryMatch, scan_dynamic_dispatch};

use super::Ctx;
use super::source::{
    find_indexed_file, has_drifted_on_disk, max_file_size, read_indexed_file_text,
};

/// Bodies scanned per question, and the characters read across them.
const MAX_SCAN: usize = 8;
const MAX_TOTAL_CHARS: usize = 200_000;
/// Candidates per site, and the search hits the shortlist draws from.
const MAX_CANDIDATES: usize = 4;
const CANDIDATE_SEARCH_LIMIT: i64 = 12;
/// The biggest file a body is read from.
const MAX_BODY_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BoundaryCandidate {
    pub node: Node,
    /// Usually the qualified name; for a typed-bus key, `Class.handlerMethod`.
    pub display: String,
    /// The reader already named this symbol.
    pub named: bool,
}

#[derive(Debug, Clone)]
pub struct BoundarySite {
    pub matched: BoundaryMatch,
    pub candidates: Vec<BoundaryCandidate>,
    /// Why there is no shortlist, when a key was visible but too generic.
    pub candidate_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NodeBoundary {
    pub node: Node,
    pub sites: Vec<BoundarySite>,
}

fn is_candidate_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component | NodeKind::Class
    )
}

/// `/^(handle|handleAsync|execute|executeAsync|consume|consumeAsync|run|__invoke)$/i`.
fn is_handler_method(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "handle"
            | "handleasync"
            | "execute"
            | "executeasync"
            | "consume"
            | "consumeasync"
            | "run"
            | "__invoke"
    )
}

/// Lower case, ASCII letters and digits only.
fn normalize_name(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// The text of `node`'s body, when its file is indexed, readable and current.
fn body_of(ctx: &Ctx<'_>, node: &Node) -> Option<String> {
    let (record, stored) = find_indexed_file(ctx, &node.file_path).ok()??;
    if has_drifted_on_disk(ctx, &stored, &record, max_file_size(ctx)) {
        return None;
    }
    let content = read_indexed_file_text(ctx, &stored, MAX_BODY_FILE_BYTES)?;
    let lines: Vec<&str> = content.split('\n').collect();
    let start = usize::try_from(node.start_line - 1).ok()?.min(lines.len());
    let end = usize::try_from(node.end_line).ok()?.min(lines.len());
    Some(lines[start..end.max(start)].join("\n"))
}

/// Scan `scan_list`'s bodies, in order, for dispatch sites — at most
/// `max_sites` across all of them.
pub fn find_dynamic_boundaries(
    ctx: &Ctx<'_>,
    scan_list: &[Node],
    named: &OrderedMap<Node>,
    max_sites: usize,
) -> Vec<NodeBoundary> {
    let mut out: Vec<NodeBoundary> = Vec::new();
    let mut seen_node: HashSet<String> = HashSet::new();
    let mut seen_site: HashSet<String> = HashSet::new();
    let mut sites = 0;
    let mut scanned = 0;
    let mut chars_scanned = 0;
    for node in scan_list {
        if sites >= max_sites || scanned >= MAX_SCAN || chars_scanned > MAX_TOTAL_CHARS {
            break;
        }
        if seen_node.contains(&node.id) || node.start_line <= 0 || node.end_line <= 0 {
            continue;
        }
        seen_node.insert(node.id.clone());
        let Some(body) = body_of(ctx, node) else {
            continue;
        };
        scanned += 1;
        chars_scanned += body.encode_utf16().count();
        let mut found: Vec<BoundarySite> = Vec::new();
        for matched in scan_dynamic_dispatch(&body, node.language.as_str(), node.start_line) {
            if sites >= max_sites {
                break;
            }
            if !seen_site.insert(format!(
                "{}:{}:{}",
                node.file_path, matched.line, matched.form
            )) {
                continue;
            }
            let (candidates, candidate_note) = match &matched.key {
                Some(key) => {
                    shortlist_boundary_candidates(ctx, key, matched.key_is_type, named, &node.id)
                }
                None => (Vec::new(), None),
            };
            found.push(BoundarySite {
                matched,
                candidates,
                candidate_note,
            });
            sites += 1;
        }
        if !found.is_empty() {
            out.push(NodeBoundary {
                node: node.clone(),
                sites: found,
            });
        }
    }
    out
}

/// Runtime targets for a dispatch key: the conventional exact names first
/// (`save` → `onSave` / `handleSave`; `CreateCmd` → `CreateCmdHandler`), then
/// full-text search, kept where one normalised name contains the other.
pub fn shortlist_boundary_candidates(
    ctx: &Ctx<'_>,
    key: &str,
    key_is_type: bool,
    named: &OrderedMap<Node>,
    self_id: &str,
) -> (Vec<BoundaryCandidate>, Option<String>) {
    let key_norm = normalize_name(key);
    if key_norm.len() < 3 {
        return (Vec::new(), None);
    }
    let mut cands: Vec<Node> = Vec::new();
    let mut consider = |n: &Node| {
        if n.id == self_id || !is_candidate_kind(n.kind) || cands.iter().any(|c| c.id == n.id) {
            return;
        }
        let name_norm = normalize_name(&n.name);
        if name_norm.len() < 3 || (!name_norm.contains(&key_norm) && !key_norm.contains(&name_norm))
        {
            return;
        }
        cands.push(n.clone());
    };

    let mut chars = key.chars();
    let cap = match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    };
    let probes: Vec<String> = if key_is_type {
        vec![format!("{key}Handler"), key.to_string()]
    } else {
        vec![
            key.to_string(),
            format!("on{cap}"),
            format!("handle{cap}"),
            format!("{key}Handler"),
            format!("handle_{key}"),
        ]
    };
    for probe in &probes {
        if let Ok(mut nodes) = ctx.store.nodes_by_name(probe) {
            nodes.sort_by(|a, b| {
                a.file_path
                    .cmp(&b.file_path)
                    .then_with(|| a.start_line.cmp(&b.start_line))
                    .then_with(|| a.id.cmp(&b.id))
            });
            for n in &nodes {
                consider(n);
            }
        }
    }
    let mut raw = 0;
    let options = SearchOptions {
        limit: Some(CANDIDATE_SEARCH_LIMIT),
        ..SearchOptions::default()
    };
    if let Ok(results) = search_nodes(ctx.store, key, &options, &HashSet::new()) {
        raw = results.len();
        for r in &results {
            consider(&r.node);
        }
    }

    if cands.is_empty() {
        let generic = raw as i64 >= CANDIDATE_SEARCH_LIMIT && key.encode_utf16().count() < 5;
        let note =
            generic.then(|| format!("key `{key}` is too generic to shortlist ({raw}+ matches)"));
        return (Vec::new(), note);
    }

    // A constructor candidate duplicates its class (constructors are METHOD
    // nodes named like the class) — keep the class.
    let class_key: HashSet<String> = cands
        .iter()
        .filter(|n| n.kind == NodeKind::Class)
        .map(|n| format!("{}|{}", n.name, n.file_path))
        .collect();
    // The flow's named set holds callables only, so a class whose METHOD the
    // reader named still counts as named — transfer the mark by name.
    let named_names: HashSet<&str> = named.values().map(|n| n.name.as_str()).collect();
    let is_named = |n: &Node| named.contains_key(&n.id) || named_names.contains(n.name.as_str());

    let mut list: Vec<Node> = cands
        .into_iter()
        .filter(|n| {
            !(n.kind != NodeKind::Class
                && class_key.contains(&format!("{}|{}", n.name, n.file_path)))
        })
        .collect();
    list.sort_by_key(|n| !is_named(n));
    list.truncate(MAX_CANDIDATES);
    let candidates = list
        .into_iter()
        .map(|n| {
            let named = is_named(&n);
            // Typed-bus convention: the runtime target is the class's
            // Handle/Execute/Consume method — name that exact node.
            if key_is_type
                && n.kind == NodeKind::Class
                && let Some(method) = handler_method_of(ctx, &n)
            {
                return BoundaryCandidate {
                    display: format!("{}.{}", n.name, method.name),
                    node: method,
                    named,
                };
            }
            BoundaryCandidate {
                display: if n.qualified_name.is_empty() {
                    n.name.clone()
                } else {
                    n.qualified_name.clone()
                },
                node: n,
                named,
            }
        })
        .collect();
    (candidates, None)
}

fn handler_method_of(ctx: &Ctx<'_>, class: &Node) -> Option<Node> {
    let edges = ctx
        .store
        .edges_by_source_kind(&class.id, Some(EdgeKind::Contains))
        .ok()?;
    let ids: Vec<String> = edges.iter().map(|e| e.target.clone()).collect();
    let nodes = ctx.store.nodes_by_ids(&ids).ok()?;
    edges
        .iter()
        .filter_map(|e| nodes.get(&e.target))
        .find(|c| c.kind == NodeKind::Method && is_handler_method(&c.name))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalise_to_ascii_letters_and_digits() {
        assert_eq!(normalize_name("handle_Save-2"), "handlesave2");
        assert_eq!(normalize_name("été"), "t");
    }

    #[test]
    fn handler_methods_follow_the_typed_bus_convention() {
        for name in [
            "Handle",
            "handleAsync",
            "EXECUTE",
            "consume",
            "run",
            "__invoke",
        ] {
            assert!(is_handler_method(name), "{name}");
        }
        assert!(!is_handler_method("handler"));
    }
}
