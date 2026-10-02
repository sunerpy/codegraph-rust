//! `GET /api/filecode/<path>` — one file, line by line: every call site, the
//! references that leave the index, and the calls that stay inside the file.
//! No source text: the viewer pages it from `/api/source`. Upstream
//! `src/ui-server/api/filecode.ts`.

use std::collections::HashMap;
use std::time::Instant;

use codegraph_core::types::{Edge, EdgeKind, NodeKind};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::file::build_outline_entries;
use super::source::{max_file_size, read_file_shape, resolve_requested_file};
use super::wire::*;
use crate::respond::{ApiError, ApiResult};

pub const MAX_FILE_CALL_GROUPS: usize = 2000;
pub const MAX_FILE_OUTSIDE_REFS: usize = 3000;
pub const MAX_FILE_OUTSIDE_SCAN: i64 = 50_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireFileCall {
    owner_id: String,
    owner_line: i64,
    relation: WireRelation,
}

fn is_plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

pub fn build(ctx: &Ctx<'_>, requested: &str) -> ApiResult<Value> {
    let started = Instant::now();
    if requested.is_empty() {
        return Err(ApiError::bad_request(
            "No file path was given. Use /api/filecode/<path>.",
        ));
    }
    let (record, stored, _) = resolve_requested_file(ctx, requested)?;
    let posix = to_posix_path(&stored);
    let nodes = ctx.store.nodes_by_file_path(&stored)?;
    let file_node = nodes.iter().find(|n| n.kind == NodeKind::File);
    let (outline, outline_total) = build_outline_entries(ctx, &nodes)?;

    // Calls: one row per (calling symbol, called symbol), in call-site order.
    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let line_of: HashMap<&str, i64> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.start_line))
        .collect();
    let edges: Vec<Edge> = ctx
        .store
        .outgoing_edges_from(&node_ids, &[])?
        .into_iter()
        .filter(|e| e.kind != EdgeKind::Contains)
        .collect();
    let mut calls: Vec<WireFileCall> = Vec::new();
    let mut call_total = 0;
    let mut intra_file_calls = 0;
    if !edges.is_empty() {
        let mut order: Vec<String> = Vec::new();
        let mut by_source: HashMap<String, Vec<&Edge>> = HashMap::new();
        for edge in &edges {
            by_source
                .entry(edge.source.clone())
                .or_insert_with(|| {
                    order.push(edge.source.clone());
                    Vec::new()
                })
                .push(edge);
        }
        let targets: Vec<String> = edges.iter().map(|e| e.target.clone()).collect();
        let endpoints = ctx.store.nodes_by_ids(&targets)?;
        let mut all: Vec<WireFileCall> = Vec::new();
        for owner in order {
            let group = by_source.remove(&owner).unwrap_or_default();
            for relation in group_relations(&group, |e| e.target.clone(), &endpoints) {
                all.push(WireFileCall {
                    owner_line: line_of.get(owner.as_str()).copied().unwrap_or(0),
                    owner_id: owner.clone(),
                    relation,
                });
            }
        }
        all.sort_by(|a, b| {
            first_line(&a.relation)
                .cmp(&first_line(&b.relation))
                .then_with(|| a.owner_line.cmp(&b.owner_line))
                .then_with(|| locale_compare(&a.relation.node.name, &b.relation.node.name))
        });
        call_total = all.len();
        calls = all.into_iter().take(MAX_FILE_CALL_GROUPS).collect();
        for call in &calls {
            if call.relation.node.file != posix {
                continue;
            }
            let target = call.relation.node.line;
            intra_file_calls += call.relation.lines.iter().filter(|l| **l != target).count();
        }
    }

    // References with nothing behind them: hollow ports.
    let mut outside_items: Vec<Value> = Vec::new();
    let mut outside_total = 0;
    if let Ok(raw) = ctx
        .store
        .unresolved_refs_in_file(&stored, MAX_FILE_OUTSIDE_SCAN)
    {
        for r in raw {
            let name = r
                .reference_name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_string();
            if !is_plain_ident(&name) || r.line == 0 {
                continue;
            }
            outside_total += 1;
            if outside_items.len() < MAX_FILE_OUTSIDE_REFS {
                outside_items.push(json!({
                    "line": r.line, "col": r.col, "name": name, "kind": r.reference_kind.as_str(),
                }));
            }
        }
    }

    let shape = read_file_shape(ctx, &stored, &record, max_file_size(ctx));
    let mut payload = json!({
        "file": {
            "path": posix,
            "language": record.language.as_str(),
            "size": record.size,
            "indexedAt": record.indexed_at,
            "contentHash": record.content_hash,
            "generated": record.generated,
            "test": is_test_file(&posix),
            "errors": record.errors,
            "id": file_node.map(|n| n.id.clone()),
            "totalLines": shape.total_lines,
        },
        "drift": shape.drift,
        "outline": wire_list(outline, outline_total),
        "calls": wire_list(calls, call_total),
        "outside": wire_list(outside_items, outside_total),
        "intraFileCalls": intra_file_calls,
        "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
    });
    if let Some(reason) = shape.reason {
        payload["reason"] = json!(reason);
    }
    Ok(payload)
}
