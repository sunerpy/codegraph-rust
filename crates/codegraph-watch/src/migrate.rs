//! Forced full migration of an index namespace that cannot be updated incrementally.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md` lines 557-565: an incremental
//! sync classifies BEFORE mutating a row. `Missing`, `Outdated`, and a
//! recoverable `Building` are not states a file-by-file update can repair, so the
//! sync escalates HERE, reusing the SAME retained exclusive lease its
//! [`StoreWriteAuthorization`] carries — no lease is released and reacquired, and
//! no nested lock is taken.
//!
//! Migration is a from-source rebuild, so it structurally
//!
//! - bypasses every mtime and content-hash skip (it reports zero unchanged
//!   skips even when every source byte matches the outdated database),
//! - processes every current candidate in sorted `scan_project` order,
//! - drops tracked files that no longer exist on disk (they are simply not
//!   candidates, so they never enter the fresh database),
//! - reruns framework extraction, resolution, and maintenance, and
//! - publishes `phase=current` LAST through the rebuild finalizer.
//!
//! Its five canonical surfaces therefore equal a fresh `index --force`: the
//! persist order below reproduces the CLI full-index pipeline exactly — file row
//! then nodes per file in sorted order, ALL nodes before ANY edge, then edges,
//! then unresolved refs, then framework extraction, batched resolution, and
//! cross-file finalization.
//!
//! Unlike the CLI's streaming full index, which spills edges/refs to a temporary
//! file, this path buffers them in memory for the duration of one migration.
//! That keeps the crate dependency-free while preserving the exact insert order;
//! the memory profile matches the pre-existing `extract_project` API and applies
//! only to the rare extraction-version migration, not to routine indexing.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use codegraph_core::IndexPaths;
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{Edge, FileRecord, Node, UnresolvedRef};
use codegraph_extract::{detect_language_with, extract_source_with};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::StoreWriteAuthorization;

use crate::sync::{ProjectScope, SyncOutcome, modified_millis, now_millis};

/// Batch sizes and the resolver batch, matching the CLI full-index constants so a
/// migrated index is byte-equal to `index --force`.
const NODE_FLUSH_ROWS: usize = 10_000;
const EDGE_FLUSH_ROWS: usize = 20_000;
const REF_FLUSH_ROWS: usize = 20_000;
const RESOLVE_BATCH_ROWS: usize = 5_000;

/// The version key an index records for `codegraph status`. Same key and same
/// workspace version the CLI full index writes.
const INDEXED_WITH_VERSION_KEY: &str = "indexed_with_version";

/// Run one forced full migration under the exclusive lease `authorization`
/// already holds.
///
/// `authorization` must be the `FullRebuildRequired` capability the incremental
/// sync's state gate returned; the rebuild layer revalidates it and refuses any
/// state it did not accept for escalation.
pub(crate) fn migrate_project(
    project_root: &Path,
    paths: &IndexPaths,
    authorization: StoreWriteAuthorization,
    scope: &ProjectScope,
    started: Instant,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    // The addressed project's own scan/extract options and framework config; the
    // migration never consults a process-global value.
    let options = &scope.options;
    // `scan_project` returns a SORTED list, and every downstream pass keeps that
    // order, so no HashSet iteration order can reach the database or the outcome.
    let candidates = codegraph_extract::engine::scan_project(project_root, options)?;
    let total = candidates.len();

    // Publishes `phase=building` BEFORE deleting a database byte, removes only
    // the database files, and opens the single fresh writer — all under the
    // lease the authorization retains.
    let rebuild = codegraph_store::resume_full_rebuild(paths, authorization)?;
    let mut rebuild = rebuild.open_store()?;
    rebuild.set_bulk_index_pragmas()?;

    let mut pending_nodes: Vec<Node> = Vec::with_capacity(NODE_FLUSH_ROWS);
    let mut edges: Vec<Edge> = Vec::new();
    let mut refs: Vec<UnresolvedRef> = Vec::new();
    let mut outcome = SyncOutcome {
        files_checked: total,
        ..SyncOutcome::default()
    };

    for (done, relative) in candidates.iter().enumerate() {
        let full = project_root.join(relative);
        // One metadata + one source read per file, mirroring the CLI producer so
        // the oversized-file skip message is byte-identical.
        let metadata = std::fs::metadata(&full)
            .with_context(|| format!("reading metadata for {}", full.display()))?;
        let source = std::fs::read_to_string(&full)
            .with_context(|| format!("reading source file {}", full.display()))?;
        let mut result = if metadata.len() > options.max_file_size {
            codegraph_core::types::ExtractionResult {
                nodes: Vec::new(),
                edges: Vec::new(),
                unresolved_references: Vec::new(),
                errors: vec![format!(
                    "File exceeds max size ({} > {}): {relative}",
                    metadata.len(),
                    options.max_file_size
                )],
                duration_ms: 0,
            }
        } else {
            extract_source_with(relative, &source, None, &options.extensions)
        };
        let file = FileRecord {
            path: relative.clone(),
            content_hash: hash_content(&source),
            language: detect_language_with(relative, &options.extensions),
            size: metadata.len() as i64,
            modified_at: modified_millis(&metadata),
            indexed_at: now_millis(),
            node_count: result
                .nodes
                .iter()
                .filter(|node| node.file_path == *relative)
                .count() as i64,
            errors: result.errors.clone(),
        };

        rebuild.store().upsert_file(&file)?;
        pending_nodes.append(&mut result.nodes);
        if pending_nodes.len() >= NODE_FLUSH_ROWS {
            rebuild.store_mut().upsert_nodes(&pending_nodes)?;
            pending_nodes.clear();
        }
        edges.append(&mut result.edges);
        refs.append(&mut result.unresolved_references);

        // Every candidate counts as reindexed: migration has no skip gate at all.
        outcome.files_reindexed += 1;
        on_progress(done + 1, total);
    }

    if !pending_nodes.is_empty() {
        rebuild.store_mut().upsert_nodes(&pending_nodes)?;
    }
    drop(pending_nodes);

    // ALL nodes are persisted before ANY edge, because `insert_edges` drops edges
    // whose endpoints are absent. The WAL valve folds the log back during the
    // replay passes exactly as the CLI full index does.
    let wal_valve_bytes = codegraph_store::wal_valve_threshold_bytes();
    for batch in edges.chunks(EDGE_FLUSH_ROWS) {
        rebuild.store_mut().insert_edges(batch)?;
        rebuild.checkpoint_wal_if_over(wal_valve_bytes)?;
    }
    drop(edges);
    for batch in refs.chunks(REF_FLUSH_ROWS) {
        rebuild.store_mut().insert_unresolved_refs(batch)?;
        rebuild.checkpoint_wal_if_over(wal_valve_bytes)?;
    }
    drop(refs);

    let mut resolver = ReferenceResolver::new(project_root.to_string_lossy());
    {
        let context = codegraph_resolve::StoreResolutionContext::new(
            rebuild.store(),
            project_root.to_string_lossy(),
        );
        resolver.initialize(&context);
    }
    if resolver.has_framework_resolvers() {
        let relative_files = rebuild
            .store()
            .all_files()?
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>();
        resolver.extract_and_persist_frameworks_with(
            rebuild.store_mut(),
            &relative_files,
            &scope.framework,
            &options.extensions,
        )?;
    }
    resolver.resolve_and_persist_batched(rebuild.store_mut(), RESOLVE_BATCH_ROWS)?;
    resolver.run_post_extract(rebuild.store_mut())?;
    rebuild
        .store()
        .set_project_metadata(INDEXED_WITH_VERSION_KEY, env!("CARGO_PKG_VERSION"))?;

    // Explicit fallible finalization: pragma restore, checkpoint + compaction,
    // extraction stamp, stamp checkpoint, connection close, and only then the
    // atomic `phase=current` publication.
    rebuild.finish()?;

    outcome.duration_ms = started.elapsed().as_millis();
    // Deterministic, sorted, and free of any HashSet iteration order.
    outcome.changed_paths = candidates;
    Ok(outcome)
}
