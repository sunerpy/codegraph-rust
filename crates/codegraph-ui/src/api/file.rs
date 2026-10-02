//! `GET /api/file/<path>` — one file: its outline, the files it imports and is
//! imported by, its broader dependencies, and whether it runs anything at its
//! top level. Upstream `src/ui-server/api/file.ts`.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::source::{has_drifted_on_disk, max_file_size, resolve_requested_file};
use super::wire::*;
use crate::respond::ApiResult;

const MAX_SYMBOLS_PER_IMPORT: usize = 12;
const MAX_UNRESOLVED_IMPORTS: usize = 60;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireOutlineEntry {
    #[serde(flatten)]
    pub node: WireNodeRef,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub fan_in: i64,
    pub fan_out: i64,
}

pub fn build(ctx: &Ctx<'_>, requested: &str) -> ApiResult<Value> {
    let (record, stored, _) = resolve_requested_file(ctx, requested)?;
    let store = ctx.store;
    let nodes = store.nodes_by_file_path(&stored)?;
    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let in_this_file: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let kind_by_id: HashMap<&str, NodeKind> =
        nodes.iter().map(|n| (n.id.as_str(), n.kind)).collect();
    let file_node = nodes.iter().find(|n| n.kind == NodeKind::File);

    let (outline, outline_total) = build_outline_entries(ctx, &nodes)?;

    let imports_out = store.outgoing_edges_from(&node_ids, &[EdgeKind::Imports])?;
    let imports_in = store.incoming_edges_to(&node_ids, &[EdgeKind::Imports])?;
    let mut endpoint_ids: Vec<String> = Vec::new();
    for edge in &imports_out {
        if !in_this_file.contains(edge.target.as_str()) {
            endpoint_ids.push(edge.target.clone());
        }
    }
    for edge in &imports_in {
        if !in_this_file.contains(edge.source.as_str()) {
            endpoint_ids.push(edge.source.clone());
        }
    }
    let endpoints = store.nodes_by_ids(&endpoint_ids)?;
    let imports = group_by_file(
        imports_out
            .iter()
            .filter(|e| !in_this_file.contains(e.target.as_str())),
        |e| e.target.clone(),
        &endpoints,
    );
    let imported_by = group_by_file(
        imports_in
            .iter()
            .filter(|e| !in_this_file.contains(e.source.as_str())),
        |e| e.source.clone(),
        &endpoints,
    );

    let unresolved_imports: Vec<Value> = match file_node {
        Some(file) => unresolved_imports_of(ctx, &file.id),
        None => Vec::new(),
    };

    let top_level_calls = match file_node {
        Some(file) => {
            let mut sources = vec![file.id.clone()];
            for edge in
                store.outgoing_edges_from(std::slice::from_ref(&file.id), &[EdgeKind::Contains])?
            {
                if matches!(
                    kind_by_id.get(edge.target.as_str()),
                    Some(NodeKind::Variable | NodeKind::Constant)
                ) {
                    sources.push(edge.target);
                }
            }
            let edges =
                store.outgoing_edges_from(&sources, &[EdgeKind::Calls, EdgeKind::Instantiates])?;
            edges
                .iter()
                .map(|e| {
                    format!(
                        "{}:{}:{}",
                        e.target,
                        e.line.unwrap_or(0),
                        e.col.unwrap_or(0)
                    )
                })
                .collect::<HashSet<_>>()
                .len()
        }
        None => 0,
    };

    let posix = to_posix_path(&stored);
    let mut dependencies: Vec<String> = store
        .dependency_file_paths(&stored)?
        .iter()
        .map(|p| to_posix_path(p))
        .collect();
    dependencies.sort();
    let mut dependents: Vec<String> = store
        .dependent_file_paths(&stored)?
        .iter()
        .map(|p| to_posix_path(p))
        .collect();
    dependents.sort();
    let imports_total = imports.len();
    let imported_by_total = imported_by.len();

    Ok(json!({
        "file": {
            "path": posix,
            "language": record.language.as_str(),
            "size": record.size,
            "modifiedAt": record.modified_at,
            "indexedAt": record.indexed_at,
            "contentHash": record.content_hash,
            "nodeCount": record.node_count,
            "generated": record.generated,
            "test": is_test_file(&posix),
            "errors": record.errors,
            "id": file_node.map(|n| n.id.clone()),
        },
        "topLevel": { "calls": top_level_calls },
        "drift": has_drifted_on_disk(ctx, &stored, &record, max_file_size(ctx)),
        "outline": wire_list(outline, outline_total),
        "imports": wire_list(imports.into_iter().take(MAX_IMPORT_FILES).collect(), imports_total),
        "importedBy": wire_list(imported_by.into_iter().take(MAX_IMPORT_FILES).collect(), imported_by_total),
        "unresolvedImports": unresolved_imports,
        "dependencies": dependencies,
        "dependents": dependents,
    }))
}

/// The outline: every symbol in the file except the file node and import
/// declarations, in source order, with its in-file parent and depth.
pub fn build_outline_entries(
    ctx: &Ctx<'_>,
    nodes: &[Node],
) -> ApiResult<(Vec<WireOutlineEntry>, usize)> {
    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let in_this_file: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let file_node_id = nodes
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .map(|n| n.id.clone());
    let mut parent_of: HashMap<String, String> = HashMap::new();
    for edge in ctx
        .store
        .outgoing_edges_from(&node_ids, &[EdgeKind::Contains])?
    {
        if in_this_file.contains(edge.target.as_str()) && !parent_of.contains_key(&edge.target) {
            parent_of.insert(edge.target.clone(), edge.source.clone());
        }
    }
    let fan_in = ctx.store.count_incoming_edges(&node_ids)?;
    let fan_out = ctx.store.count_outgoing_edges(&node_ids)?;
    let mut outline: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File && n.kind != NodeKind::Import)
        .collect();
    outline.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| locale_compare(&a.name, &b.name))
    });
    let total = outline.len();
    let parent_for = |id: &str| -> Option<String> {
        match parent_of.get(id) {
            Some(parent) if Some(parent) != file_node_id.as_ref() => Some(parent.clone()),
            _ => None,
        }
    };
    let depth_of = |id: &str| -> usize {
        let mut depth = 0;
        let mut current = id.to_string();
        for _ in 0..32 {
            match parent_of.get(&current) {
                Some(parent) if Some(parent) != file_node_id.as_ref() => {
                    depth += 1;
                    current = parent.clone();
                }
                _ => return depth,
            }
        }
        depth
    };
    let entries = outline
        .into_iter()
        .take(MAX_OUTLINE_NODES)
        .map(|node| WireOutlineEntry {
            node: to_node_ref(node),
            parent_id: parent_for(&node.id),
            depth: depth_of(&node.id),
            fan_in: fan_in.get(&node.id).copied().unwrap_or(0),
            fan_out: fan_out.get(&node.id).copied().unwrap_or(0),
        })
        .collect();
    Ok((entries, total))
}

fn group_by_file<'a, I, F>(edges: I, endpoint: F, nodes: &HashMap<String, Node>) -> Vec<Value>
where
    I: Iterator<Item = &'a Edge>,
    F: Fn(&Edge) -> String,
{
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<Node>> = HashMap::new();
    for edge in edges {
        let Some(node) = nodes.get(&endpoint(edge)) else {
            continue;
        };
        let file = to_posix_path(&node.file_path);
        let bucket = by_file.entry(file.clone()).or_insert_with(|| {
            order.push(file.clone());
            Vec::new()
        });
        if !bucket.iter().any(|n| n.id == node.id) {
            bucket.push(node.clone());
        }
    }
    let mut rows: Vec<(String, Vec<Node>)> = order
        .into_iter()
        .map(|file| {
            let mut symbols = by_file.remove(&file).unwrap_or_default();
            symbols.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then_with(|| locale_compare(&a.name, &b.name))
            });
            (file, symbols)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| locale_compare(&a.0, &b.0))
    });
    rows.into_iter()
        .map(|(file, symbols)| {
            json!({
                "test": is_test_file(&file),
                "file": file,
                "symbols": symbols.iter().take(MAX_SYMBOLS_PER_IMPORT).map(|n| json!({
                    "id": n.id, "name": n.name, "kind": n.kind.as_str(), "line": n.start_line,
                })).collect::<Vec<_>>(),
                "symbolCount": symbols.len(),
            })
        })
        .collect()
}

fn unresolved_imports_of(ctx: &Ctx<'_>, file_node_id: &str) -> Vec<Value> {
    let Ok(refs) = ctx.store.unresolved_refs_from(file_node_id) else {
        return Vec::new();
    };
    let mut imports: Vec<_> = refs
        .into_iter()
        .filter(|r| r.reference_kind == EdgeKind::Imports)
        .collect();
    imports.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| locale_compare(&a.reference_name, &b.reference_name))
    });
    imports
        .into_iter()
        .take(MAX_UNRESOLVED_IMPORTS)
        .map(|r| json!({ "name": r.reference_name, "line": r.line }))
        .collect()
}
