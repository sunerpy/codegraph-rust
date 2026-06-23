use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::FileRecord;
use codegraph_extract::{detect_language, extract_file, ExtractOptions};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

use crate::policy::WatchPolicy;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub files_checked: usize,
    pub files_reindexed: usize,
    pub files_skipped_unchanged: usize,
    pub files_removed: usize,
    pub files_ignored: usize,
    pub duration_ms: u128,
}

pub fn sync_project_once(project_root: impl AsRef<Path>) -> Result<SyncOutcome> {
    sync_project_once_with_progress(project_root, |_, _| {})
}

/// Like [`sync_project_once`] but invokes `on_progress(done, total)` after each
/// candidate file is processed, letting a caller drive a progress bar. The
/// callback is a pure side effect: it never gates or reorders work, so the
/// result stays byte-equivalent to `index --force`.
pub fn sync_project_once_with_progress(
    project_root: impl AsRef<Path>,
    on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    let project_root = project_root.as_ref();
    let config = codegraph_core::config::get_config();
    let options = ExtractOptions {
        max_file_size: config.indexing.max_file_size,
        ignore_dirs: config.indexing.ignore_dirs.clone(),
        parallel: true,
    };
    let started = std::time::Instant::now();
    let mut candidates = codegraph_extract::engine::scan_project(project_root, &options)?;

    let db_path = default_db_path(project_root);
    let mut store = Store::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;

    // Cold CLI sync has no watcher event list, so deletions are found by diffing
    // tracked files against scan_project's on-disk set; absent paths flow through
    // sync_one's delete branch (upstream removal pass, index.ts:1436-1441). The
    // `exists()` guard keeps a still-present file that merely became ignored.
    let on_disk = candidates.iter().cloned().collect::<HashSet<_>>();
    for tracked in store.all_files()? {
        if !on_disk.contains(&tracked.path) && !project_root.join(&tracked.path).exists() {
            candidates.push(tracked.path);
        }
    }

    sync_paths_with_store(&mut store, project_root, candidates, started, on_progress)
}

pub fn sync_changed_paths(
    project_root: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<SyncOutcome> {
    let started = std::time::Instant::now();
    let project_root = project_root.as_ref();
    let db_path = db_path.as_ref();
    let mut store = Store::open(db_path).with_context(|| format!("open {}", db_path.display()))?;
    sync_paths_with_store(&mut store, project_root, paths, started, |_, _| {})
}

fn sync_paths_with_store(
    store: &mut Store,
    project_root: &Path,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    started: std::time::Instant,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    let policy = WatchPolicy::new(project_root);
    let mut outcome = SyncOutcome::default();
    let mut changed = false;
    let mut seen = HashSet::new();

    let mut dependents = HashSet::new();
    let mut reindexed = HashSet::new();
    let mut changed_names = HashSet::new();

    let paths = paths.into_iter().collect::<Vec<_>>();
    let total = paths.len();
    for (done, path) in paths.into_iter().enumerate() {
        let Some(relative) = policy.normalize_relative(path.as_ref()) else {
            outcome.files_ignored += 1;
            on_progress(done + 1, total);
            continue;
        };
        if !seen.insert(relative.clone()) {
            on_progress(done + 1, total);
            continue;
        }
        outcome.files_checked += 1;
        if !policy.should_handle_file(&relative) {
            outcome.files_ignored += 1;
            on_progress(done + 1, total);
            continue;
        }
        if sync_one(
            project_root,
            store,
            &relative,
            &mut outcome,
            &mut dependents,
            &mut changed_names,
        )? {
            changed = true;
            reindexed.insert(relative);
        }
        on_progress(done + 1, total);
    }

    if changed {
        let name_list: Vec<String> = changed_names.iter().cloned().collect();
        for affected in store.source_files_of_edges_to_named_targets(&name_list)? {
            if !reindexed.contains(&affected) {
                dependents.insert(affected);
            }
        }
        refresh_dependent_refs(
            project_root,
            store,
            &dependents,
            &reindexed,
            &mut changed_names,
        )?;
        let mut scope_files = reindexed.clone();
        for dependent in &dependents {
            if !reindexed.contains(dependent) {
                scope_files.insert(dependent.clone());
            }
        }
        let mut resolver = ReferenceResolver::new(project_root.to_string_lossy());
        {
            let context = codegraph_resolve::StoreResolutionContext::new(
                store,
                project_root.to_string_lossy(),
            );
            resolver.initialize(&context);
        }
        // Re-run framework per-file extract for reindexed files whose framework
        // nodes/refs were dropped on re-extraction (upstream tree-sitter.ts:4796-4819).
        if resolver.has_framework_resolvers() {
            let reindexed_files: Vec<String> = reindexed.iter().cloned().collect();
            resolver.extract_and_persist_frameworks(store, &reindexed_files)?;
        }
        resolver.resolve_incremental_and_persist(store, &scope_files, &changed_names)?;
        // Cross-file framework finalization on every sync (upstream index.ts:464).
        resolver.run_post_extract(store)?;
    }
    outcome.duration_ms = started.elapsed().as_millis();
    Ok(outcome)
}

/// Rebuild the outgoing resolved references of files affected by the change
/// WITHOUT deleting their nodes.
///
/// An affected file F is either a one-hop dependent of a changed file (it had a
/// resolved edge INTO a changed file, cascade-deleted when that file's nodes
/// went) or a file whose refs resolve to a symbol whose name's node-set changed
/// (so the resolution confidence or chosen target may differ now). In both cases
/// F's content is unchanged on disk, so F's nodes, `contains` edges, and incoming
/// edges are already identical to a full index and must NOT be touched — deleting
/// F's nodes would cascade away the edges INTO F and force the same recovery for
/// F's own dependents (an unbounded closure on hub symbols). So F is extracted in
/// memory only: all its outgoing resolved (non-`contains`) edges are dropped, its
/// `unresolved_refs` rows are refreshed, and the incremental pass re-resolves them
/// against the final graph — rebuilding exactly the edges a full `index --force`
/// would, with no duplication, no stale confidence, and no second-hop cascade.
fn refresh_dependent_refs(
    project_root: &Path,
    store: &mut Store,
    dependents: &HashSet<String>,
    already_reindexed: &HashSet<String>,
    changed_names: &mut HashSet<String>,
) -> Result<()> {
    for relative in dependents {
        if already_reindexed.contains(relative) {
            continue;
        }
        if !project_root.join(relative).exists() {
            continue;
        }
        let result = extract_file(project_root, relative)?;
        let node_ids = result
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<HashSet<_>>();
        let refs = result
            .unresolved_references
            .into_iter()
            .filter(|reference| node_ids.contains(reference.from_node_id.as_str()))
            .collect::<Vec<_>>();
        for node in &result.nodes {
            changed_names.insert(node.name.clone());
        }
        store.delete_resolved_edges_from_file(relative)?;
        delete_unresolved_refs_by_file(store, relative)?;
        store.insert_unresolved_refs(&refs)?;
    }
    Ok(())
}

pub(crate) fn default_db_path(project_root: &Path) -> PathBuf {
    project_root.join(".codegraph").join("codegraph.db")
}

fn sync_one(
    project_root: &Path,
    store: &mut Store,
    relative: &str,
    outcome: &mut SyncOutcome,
    dependents: &mut HashSet<String>,
    changed_names: &mut HashSet<String>,
) -> Result<bool> {
    let full = project_root.join(relative);
    // One stat serves as both the existence check and the (mtime, size) source.
    let metadata = match fs::metadata(&full) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            for dependent in store.dependent_file_paths(relative)? {
                dependents.insert(dependent);
            }
            for name in node_names_in_file(store, relative)? {
                changed_names.insert(name);
            }
            delete_unresolved_refs_by_file(store, relative)?;
            store.delete_file_record(relative)?;
            outcome.files_removed += 1;
            return Ok(true);
        }
    };

    let stored = store.file_by_path(relative)?;

    // Pre-filter: a tracked file whose on-disk (mtime, size) BOTH match the
    // stored record is almost-certainly unchanged, so skip the read+SHA256.
    // CORRECTNESS: this is a pre-filter only. Any mtime/size difference, and
    // every new/untracked file, falls through to the content-hash gate below,
    // which stays authoritative — keeping the DB byte-identical to `index
    // --force`. (The equivalence tests edit file content, which changes size
    // and/or mtime, so they correctly fall through and reindex.)
    if let Some(file) = &stored {
        if file.size == metadata.len() as i64 && file.modified_at == modified_millis(&metadata) {
            outcome.files_skipped_unchanged += 1;
            return Ok(false);
        }
    }

    let source = fs::read_to_string(&full).with_context(|| format!("read {}", full.display()))?;
    let content_hash = hash_content(&source);
    // Authoritative content gate, port of the upstream hash gate in
    // `upstream extraction/index.ts:1326-1337,1465-1483`.
    if stored.is_some_and(|file| file.content_hash == content_hash) {
        outcome.files_skipped_unchanged += 1;
        return Ok(false);
    }

    for dependent in store.dependent_file_paths(relative)? {
        dependents.insert(dependent);
    }
    reextract_into_store(project_root, store, relative, changed_names)?;
    outcome.files_reindexed += 1;
    Ok(true)
}

fn reextract_into_store(
    project_root: &Path,
    store: &mut Store,
    relative: &str,
    changed_names: &mut HashSet<String>,
) -> Result<()> {
    let full = project_root.join(relative);
    let source = fs::read_to_string(&full).with_context(|| format!("read {}", full.display()))?;
    let metadata = fs::metadata(&full).with_context(|| format!("stat {}", full.display()))?;
    let result = extract_file(project_root, relative)?;
    let node_ids = result
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let refs = result
        .unresolved_references
        .into_iter()
        .filter(|reference| node_ids.contains(reference.from_node_id.as_str()))
        .collect::<Vec<_>>();
    let file = FileRecord {
        path: relative.to_string(),
        content_hash: hash_content(&source),
        language: detect_language(relative),
        size: metadata.len() as i64,
        modified_at: modified_millis(&metadata),
        indexed_at: now_millis(),
        node_count: result
            .nodes
            .iter()
            .filter(|node| node.file_path == relative)
            .count() as i64,
        errors: result.errors,
    };

    // A name's resolution outcomes (confidence, chosen target) depend on the set
    // of nodes carrying that name. Only names whose node identity in THIS file
    // changed — a node id present before but not after, or vice versa — can alter
    // any ref's resolution; a name whose `(id)` set is unchanged resolves exactly
    // as before. Editing the tail of a file (no line shift for earlier symbols)
    // therefore contributes no names, keeping the re-resolve scope minimal.
    let old_nodes: HashSet<(String, String)> = store
        .nodes_by_file_path(relative)?
        .into_iter()
        .map(|node| (node.id, node.name))
        .collect();
    let new_nodes: HashSet<(String, String)> = result
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.name.clone()))
        .collect();
    for (_, name) in old_nodes.symmetric_difference(&new_nodes) {
        changed_names.insert(name.clone());
    }

    delete_unresolved_refs_by_file(store, relative)?;
    store.delete_file_record(relative)?;
    store.upsert_nodes(&result.nodes)?;
    store.insert_edges(&result.edges)?;
    store.insert_unresolved_refs(&refs)?;
    store.upsert_file(&file)?;
    Ok(())
}

fn node_names_in_file(store: &Store, relative: &str) -> Result<HashSet<String>> {
    Ok(store
        .nodes_by_file_path(relative)?
        .into_iter()
        .map(|node| node.name)
        .collect())
}

fn delete_unresolved_refs_by_file(store: &Store, relative: &str) -> rusqlite::Result<usize> {
    store.connection().execute(
        "DELETE FROM unresolved_refs WHERE file_path = ?",
        [relative],
    )
}

fn modified_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_else(now_millis)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    pub(crate) struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        pub(crate) fn new(name: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("codegraph-{name}-{id}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        pub(crate) fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn unchanged_file_hash_is_not_reindexed() {
        let dir = TestDir::new("watch-skip");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path());

        let first = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(first.files_reindexed, 1);
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        let second = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(second.files_reindexed, 0);
        assert_eq!(second.files_skipped_unchanged, 1);
    }
}
