//! `GET /api/stats` — index state, graph counts, detected frameworks, the
//! thresholds the API applies, and the blast-radius scale. Upstream
//! `src/ui-server/api/stats.ts`.

use std::collections::BTreeMap;

use codegraph_graph::graph::GraphTraverser;
use codegraph_resolve::StoreResolutionContext;
use serde_json::{Value, json};

use super::Ctx;
use super::wire::{BLAST_DEPTH, HUB_THRESHOLD, UNCERTAIN_BELOW};
use crate::respond::ApiResult;

/// Symbols measured for the depth-3 radius denominator.
const BLAST_SCALE_SAMPLE: i64 = 24;

/// The index's revision key the memos share: last indexed, files, edges.
pub fn revision_key(ctx: &Ctx<'_>) -> ApiResult<String> {
    let (last, files) = ctx.store.index_revision()?;
    let edges = ctx.store.counts()?.edge_count;
    Ok(format!(
        "{}\u{0}{}:{files}:{edges}",
        ctx.project_root().display(),
        last.unwrap_or(0)
    ))
}

/// The denominator the Symbol view's blast bar is drawn against.
pub fn blast_scale(ctx: &Ctx<'_>) -> ApiResult<Value> {
    let key = revision_key(ctx)?;
    if let Some(hit) = ctx.state.caches.blast_scale.get(&key) {
        return Ok(hit);
    }
    let top = ctx.store.top_depended_on(BLAST_SCALE_SAMPLE)?;
    let traverser = GraphTraverser::new(ctx.store);
    let mut max_within = 0usize;
    for (node_id, _) in &top {
        // A candidate that cannot be traversed narrows the sample; it must not
        // fail the screen.
        if let Ok(subgraph) = traverser.get_impact_radius(node_id, BLAST_DEPTH) {
            max_within = max_within.max(subgraph.nodes.len().saturating_sub(1));
        }
    }
    let value = json!({
        "maxDirect": top.first().map(|(_, n)| *n).unwrap_or(0),
        "maxWithinHops": max_within,
        "hops": BLAST_DEPTH,
        "sampled": top.len(),
        "estimated": true,
    });
    ctx.state.caches.blast_scale.put(key, value.clone());
    Ok(value)
}

/// The framework resolvers this project's sources activate, memoised on the
/// index revision (detection reads manifests from disk).
pub fn frameworks(ctx: &Ctx<'_>) -> ApiResult<Vec<String>> {
    let key = revision_key(ctx)?;
    if let Some(Value::Array(names)) = ctx.state.caches.frameworks.get(&key) {
        return Ok(names
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect());
    }
    let context = StoreResolutionContext::new(ctx.store, ctx.project_root().to_string_lossy());
    let names: Vec<String> = codegraph_resolve::frameworks::detect_frameworks(&context)
        .iter()
        .map(|resolver| resolver.name().to_string())
        .collect();
    ctx.state
        .caches
        .frameworks
        .put(key, Value::from(names.clone()));
    Ok(names)
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn counts_map(rows: Vec<(String, i64)>) -> BTreeMap<String, i64> {
    rows.into_iter().collect()
}

pub fn build(ctx: &Ctx<'_>) -> ApiResult<Value> {
    let store = ctx.store;
    let counts = store.counts()?;
    let db = ctx.state.paths.current_db();
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    let built_with_version = store.get_project_metadata("indexed_with_version")?;
    let extraction_version = store
        .get_project_metadata(codegraph_store::EXTRACTION_VERSION_KEY)?
        .and_then(|v| v.parse::<u64>().ok());
    let incomplete = store.is_resolution_incomplete()?;
    let root = ctx.project_root();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());

    Ok(json!({
        "project": { "root": root.display().to_string(), "name": name },
        "index": {
            // The published namespace opened read-only, so the run that wrote it
            // finished; `partial` is the interrupted-resolution marker (#1187).
            "state": if incomplete { "partial" } else { "complete" },
            "lastIndexedAt": store.last_indexed_at()?,
            "stale": extraction_version.is_none_or(|v| v < codegraph_store::CURRENT_EXTRACTION_VERSION),
            "version": built_with_version,
            "extractionVersion": extraction_version,
            "backend": "rusqlite",
            "journalMode": store.journal_mode()?,
            "pendingReferences": store.unresolved_refs_count()?,
            "generatedFiles": store.generated_file_count()?,
            // The viewer never runs a syncing watcher of its own.
            "watching": false,
            "watcherDegraded": false,
        },
        "graph": {
            "nodes": counts.node_count,
            "edges": counts.edge_count,
            "files": counts.file_count,
            "nodesByKind": counts_map(store.node_counts_by_kind()?),
            "edgesByKind": counts_map(store.edge_counts_by_kind()?),
            "filesByLanguage": counts_map(store.file_counts_by_language()?),
            "dbSizeBytes": file_size(&db),
            "walSizeBytes": file_size(std::path::Path::new(&wal)),
        },
        "frameworks": frameworks(ctx)?,
        "thresholds": { "hub": HUB_THRESHOLD, "uncertainBelow": UNCERTAIN_BELOW },
        "blastScale": blast_scale(ctx)?,
    }))
}
