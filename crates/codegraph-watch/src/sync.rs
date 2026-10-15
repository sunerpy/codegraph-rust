use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use codegraph_core::IndexPaths;
use codegraph_core::config::Config;
use codegraph_core::generated_header::{detect_generated_file, is_minifiable_script};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{FileRecord, Language, Node, NodeKind};
use codegraph_extract::{
    ExtensionOverrides, ExtractOptions, SourceText, detect_language_of, extract_file_with_options,
    is_source_file, read_source_file,
};
use codegraph_resolve::ReferenceResolver;
use codegraph_resolve::framework::FrameworkExtractionContext;
use codegraph_resolve::frameworks::godot_dsl_config::GodotDslConfig;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};
use codegraph_store::{IndexLease, Store, StoreWriteOpen, StoreWritePurpose};

use crate::git_pending::{GitIndexCapture, PendingSource};
use crate::link_state::{RehashUnder, record_followed_links};
use crate::policy::WatchPolicy;

/// Bounded wall-clock budget for acquiring the ONE outer exclusive lease a sync
/// owns. Never a blocking wait: `IndexLease` polls `try_lock` against this
/// monotonic deadline, so a sync behind another writer fails with a typed
/// timeout instead of hanging.
const SYNC_LEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cooperative cancellation for a sync's bounded lease loop.
///
/// Frozen plan lines 598-601: `uninit --force` signals cancellation to queued and
/// running watcher lock loops and drains only after every cancellation-aware
/// lease loop has exited. Without this, a watcher sync queued behind uninit's
/// exclusive lease would spin its full [`SYNC_LEASE_TIMEOUT`] before the daemon
/// could finish draining. Cancellation is observed by `IndexLease`'s bounded
/// acquisition loop, so a cancelled sync returns a typed error and mutates
/// nothing.
#[derive(Clone, Debug, Default)]
pub struct SyncCancellation {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    running: Arc<std::sync::atomic::AtomicUsize>,
}

impl SyncCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Refuse every queued lease loop and interrupt every running one.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many cancellation-aware lease loops are currently inside a sync.
    /// A drain waits for this to reach zero after [`cancel`](Self::cancel).
    #[must_use]
    pub fn active_syncs(&self) -> usize {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn enter(&self) -> SyncCancellationGuard<'_> {
        self.running
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SyncCancellationGuard(self)
    }
}

struct SyncCancellationGuard<'a>(&'a SyncCancellation);

impl Drop for SyncCancellationGuard<'_> {
    fn drop(&mut self) {
        self.0
            .running
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub files_checked: usize,
    pub files_reindexed: usize,
    pub files_skipped_unchanged: usize,
    pub files_removed: usize,
    pub files_ignored: usize,
    pub duration_ms: u128,
    /// Reindexed or removed paths this sync, root-relative and SORTED — the
    /// source set is a `HashSet`, so sorting is required for a stable log line.
    pub changed_paths: Vec<String>,
    /// Root-relative paths that triggered a watcher sync, sorted and deduplicated
    /// by the watcher. Empty for direct/full sync callers. Unlike `changed_paths`,
    /// this remains populated when another writer (for example startup catch-up)
    /// indexed the same event first and this sync consequently skips it unchanged.
    pub trigger_paths: Vec<String>,
    /// References the orphan sweep attempted, resolved and left unresolved
    /// when it ran (upstream #1360), so a sync that healed an interrupted index
    /// does not read as a no-op. Zero on a healthy index.
    pub pending_refs_processed: usize,
    pub pending_refs_resolved: usize,
    pub pending_refs_unresolved: usize,
}

/// Read-only difference between the current project scope and the persisted
/// file inventory. Paths are root-relative, sorted, and mutually exclusive.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PendingChanges {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

impl PendingChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.added.len() + self.modified.len() + self.removed.len()
    }
}

/// Compute the same whole-project candidate delta an ordinary full `sync` would
/// inspect, without acquiring a writer lease or mutating any index byte (#1829).
///
/// This intentionally uses the correctness-first full scan instead of Git's
/// working-tree status: a committed change is absent from `git status`, history
/// can be rebased/shallow, and CodeGraph also supports non-Git projects. The
/// scan reuses the addressed project's include/exclude/custom-extension policy.
/// Existing files use sync's `(size, mtime) -> content hash` decision, so the
/// reported counts match the work the next whole-tree sync will perform.
pub fn pending_project_changes(
    project_root: impl AsRef<Path>,
    store: &Store,
) -> Result<PendingChanges> {
    Ok(pending_project_changes_detailed(project_root, store)?.0)
}

/// [`pending_project_changes`], and which computation answered: the git fast
/// path when git can see every change since the index was built (#1878),
/// otherwise the full inventory.
pub fn pending_project_changes_detailed(
    project_root: impl AsRef<Path>,
    store: &Store,
) -> Result<(PendingChanges, PendingSource)> {
    let project_root = project_root.as_ref();
    let paths = index_paths(project_root)?;
    let scope = ProjectScope::load(project_root, &paths)?;
    if let Some(fast) = crate::git_pending::git_fast_pending(project_root, store, &scope.options)? {
        return Ok((fast, PendingSource::GitFastPath));
    }
    Ok((
        full_inventory(project_root, store, &scope)?,
        PendingSource::FullInventory,
    ))
}

/// The reference answer: a full scan against the persisted inventory, never
/// consulting git.
pub fn pending_full_inventory(
    project_root: impl AsRef<Path>,
    store: &Store,
) -> Result<PendingChanges> {
    let project_root = project_root.as_ref();
    let paths = index_paths(project_root)?;
    let scope = ProjectScope::load(project_root, &paths)?;
    full_inventory(project_root, store, &scope)
}

fn full_inventory(
    project_root: &Path,
    store: &Store,
    scope: &ProjectScope,
) -> Result<PendingChanges> {
    let scan = codegraph_extract::engine::scan_project_with_stats(project_root, &scope.options)?;
    // A path reached through a new, retargeted or unrecorded link is re-read:
    // its stored stat may describe a different file (#935).
    let rehash = RehashUnder::for_scan(store, &scan.links)?;
    let on_disk = scan.files;
    let tracked = store
        .all_files()?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let on_disk_set = on_disk.iter().cloned().collect::<BTreeSet<_>>();

    let max_file_size = scope.options.max_file_size;
    let mut pending = PendingChanges::default();
    for relative in on_disk {
        let full = project_root.join(&relative);
        let Some(stored) = tracked.get(&relative) else {
            // An untracked video clip named `.ts` is not source sync would add.
            if is_source_file(&full, &relative, max_file_size)
                .with_context(|| format!("read pending source {}", full.display()))?
            {
                pending.added.push(relative);
            }
            continue;
        };
        let metadata = fs::metadata(&full).with_context(|| format!("stat {}", full.display()))?;
        if stored.size == metadata.len() as i64
            && stored.modified_at == modified_millis(&metadata)
            && !rehash.covers(&relative)
        {
            continue;
        }
        let (_, source) = read_source_file(&full, &relative, max_file_size)
            .with_context(|| format!("read pending source {}", full.display()))?;
        match source.hash_input() {
            // A tracked file that became a video clip leaves the index.
            None => pending.removed.push(relative),
            Some(hash_input) if stored.content_hash != hash_content(&hash_input) => {
                pending.modified.push(relative);
            }
            Some(_) => {}
        }
    }
    for relative in tracked.keys() {
        if !on_disk_set.contains(relative) {
            pending.removed.push(relative.clone());
        }
    }

    pending.added.sort();
    pending.modified.sort();
    pending.removed.sort();
    Ok(pending)
}

pub fn sync_project_once(project_root: impl AsRef<Path>) -> Result<SyncOutcome> {
    sync_project_once_with_progress(project_root, |_, _| {})
}

/// [`sync_project_once`] under a caller-owned [`SyncCancellation`], so a daemon
/// shutdown can refuse this sync's queued lease loop and interrupt a running one
/// instead of waiting out the full lease budget.
pub fn sync_project_once_cancellable(
    project_root: impl AsRef<Path>,
    cancel: &SyncCancellation,
) -> Result<SyncOutcome> {
    let project_root = project_root.as_ref();
    let paths = index_paths(project_root)?;
    let scope = ProjectScope::load(project_root, &paths)?;
    sync_project_once_with_scope(project_root, &paths, &scope, Some(cancel), |_, _| {})
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
    let paths = index_paths(project_root)?;
    let scope = ProjectScope::load(project_root, &paths)?;
    sync_project_once_with_scope(project_root, &paths, &scope, None, on_progress)
}

fn sync_project_once_with_scope(
    project_root: &Path,
    paths: &IndexPaths,
    scope: &ProjectScope,
    cancel: Option<&SyncCancellation>,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    let options = &scope.options;
    let include = options.include.clone();
    let exclude = options.exclude.clone();
    let started = Instant::now();

    // CLASSIFY BEFORE MUTATING A ROW (frozen plan lines 557-565). The gate takes
    // the ONE outer exclusive lease, classifies under it, and either hands back
    // the incremental writer for a corroborated Current namespace or the
    // lease-retaining authorization that escalates to a forced full migration.
    // Future/Corrupt/unauthorized-Uninitialized fail here, before any byte moves.
    let _active = cancel.map(SyncCancellation::enter);
    match open_sync_writer(paths, cancel)? {
        SyncWriter::Incremental(mut store) => {
            // Forget the git record before any row moves, and capture git
            // before the scan, so the record this sync writes is race-safe.
            crate::git_pending::forget(&store)?;
            let git_capture = GitIndexCapture::begin(project_root, options);
            let scan = codegraph_extract::engine::scan_project_with_stats(project_root, options)?;
            // Re-read whatever a new, retargeted or unrecorded link reaches, so a
            // same-stat file behind a moved link cannot keep its old graph (#935).
            let rehash = RehashUnder::for_scan(&store, &scan.links)?;
            let mut candidates = scan.files;
            // Cold CLI sync has no watcher event list, so removals are found by
            // diffing tracked files against scan_project's current in-scope set.
            // This includes both physically absent files and still-present files
            // that a newly loaded include/exclude/.gitignore rule moved out of
            // scope. Both flow through sync_one's forced-absent branch.
            let on_disk = candidates.iter().cloned().collect::<HashSet<_>>();
            let mut absent = Vec::new();
            for tracked in store.all_files()? {
                if !on_disk.contains(&tracked.path) {
                    absent.push(tracked.path);
                }
            }
            // `all_files` is ordered by path, but sort explicitly so the appended
            // tail is deterministic regardless of the query's ordering.
            absent.sort();
            candidates.extend(absent);

            let outcome = sync_paths_with_store(
                &mut store,
                project_root,
                candidates,
                Some(&on_disk),
                &rehash,
                scope,
                &include,
                &exclude,
                started,
                on_progress,
            )?;
            record_followed_links(&store, &scan.links)?;
            if let Some(capture) = git_capture {
                capture.record(&store, project_root, options, !scan.links.is_empty())?;
            }
            store.finish_current_mutation()?;
            Ok(outcome)
        }
        SyncWriter::MigrationRequired(authorization) => crate::migrate::migrate_project(
            project_root,
            paths,
            authorization,
            scope,
            started,
            &mut on_progress,
        ),
    }
}

/// One project's immutable configuration for ONE sync operation.
///
/// Loaded from the addressed project's resolved [`IndexPaths`] — its current-root
/// `config.toml` (scan/extract options) and current-root `codegraph.json`
/// (extension overrides + Godot DSL fields). Nothing here reads a process-global
/// value, another project's index root, or the process working directory, so a
/// watcher, a direct sync, and a startup catch-up on different projects inside one
/// process each operate under their own settings.
pub(crate) struct ProjectScope {
    pub(crate) options: ExtractOptions,
    pub(crate) framework: FrameworkExtractionContext,
}

impl ProjectScope {
    pub(crate) fn load(project_root: &Path, paths: &IndexPaths) -> Result<Self> {
        let config = Config::load_for_paths(None, paths)?;
        let extensions = ExtensionOverrides::load_for_paths(paths);
        let framework = FrameworkExtractionContext::new(
            project_root.to_string_lossy().into_owned(),
            GodotDslConfig::load_for_paths(paths),
        );
        Ok(Self {
            options: ExtractOptions::for_project(&config, extensions),
            framework,
        })
    }
}

/// What the state gate authorized for ONE sync.
enum SyncWriter {
    /// A corroborated `Current` namespace, open for incremental mutation under
    /// the retained exclusive lease. Boxed because the state gate already yields
    /// a boxed `Store`, so this variant stays the same size as the authorization.
    Incremental(Box<Store>),
    /// A namespace that cannot be updated file-by-file. The opaque authorization
    /// still owns the same exclusive lease, which the migration reuses.
    MigrationRequired(codegraph_store::StoreWriteAuthorization),
}

pub(crate) fn index_paths(project_root: &Path) -> Result<IndexPaths> {
    Ok(IndexPaths::resolve(
        project_root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?)
}

/// Acquire the ONE outer exclusive lease and classify under it. The retained
/// status decides policy; no precheck-then-reclassify happens anywhere.
fn open_sync_writer(paths: &IndexPaths, cancel: Option<&SyncCancellation>) -> Result<SyncWriter> {
    if let Some(cancel) = cancel
        && cancel.is_cancelled()
    {
        bail!(
            "sync of {} was cancelled before acquiring the index lease",
            paths.current_root().display()
        );
    }
    let deadline = Instant::now() + SYNC_LEASE_TIMEOUT;
    let cancelled = || cancel.is_some_and(SyncCancellation::is_cancelled);
    let lease = IndexLease::acquire_or_create_exclusive(paths, deadline, cancelled)?;
    match Store::open_for_write(paths, lease, StoreWritePurpose::IncrementalSync)? {
        StoreWriteOpen::Current(store) => Ok(SyncWriter::Incremental(store)),
        StoreWriteOpen::FullRebuildRequired(authorization) => {
            Ok(SyncWriter::MigrationRequired(authorization))
        }
        StoreWriteOpen::UninitContinuation(_) => {
            // The gate never returns this for an incremental sync; keep it a
            // typed refusal rather than an unreachable panic.
            bail!(
                "index namespace {} awaits an explicit `codegraph init`",
                paths.current_root().display()
            )
        }
    }
}

pub fn sync_changed_paths(
    project_root: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<SyncOutcome> {
    sync_changed_paths_current_scope(project_root, db_path, paths, None)
}

/// Scoped sync under the addressed project's config as it exists at the start
/// of THIS operation. The watcher uses this path so queued events can never
/// re-admit a file that a newer config has excluded.
pub(crate) fn sync_changed_paths_cancellable(
    project_root: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    cancel: &SyncCancellation,
) -> Result<SyncOutcome> {
    sync_changed_paths_current_scope(project_root, db_path, paths, Some(cancel))
}

fn sync_changed_paths_current_scope(
    project_root: impl AsRef<Path>,
    db_path: impl AsRef<Path>,
    changed: impl IntoIterator<Item = impl AsRef<Path>>,
    cancel: Option<&SyncCancellation>,
) -> Result<SyncOutcome> {
    let started = Instant::now();
    let project_root = project_root.as_ref();
    let db_path = db_path.as_ref();
    let paths = index_paths(project_root)?;
    // The caller-supplied database must BE this project's resolved database.
    // Accepting any other path would let a watcher mutate a namespace the state
    // gate never classified, so a mismatch fails closed.
    if db_path != paths.current_db() {
        bail!(
            "incremental sync target {} is not the resolved database {}",
            db_path.display(),
            paths.current_db().display()
        );
    }

    // The addressed project's current config drives extraction and migration.
    // Reloading it for every incremental operation prevents a queued event from
    // re-admitting a file that a newer scope has excluded.
    let scope = ProjectScope::load(project_root, &paths)?;
    let include = scope.options.include.clone();
    let exclude = scope.options.exclude.clone();

    // Same classify-before-mutate gate as the cold full sync: a Current namespace
    // stays incremental, while Missing/Outdated/recoverable Building escalate to a
    // forced migration under the SAME retained exclusive lease.
    let _active = cancel.map(SyncCancellation::enter);
    match open_sync_writer(&paths, cancel)? {
        SyncWriter::Incremental(mut store) => {
            // Rows change only for these paths, so the git record stays valid
            // once they are in its always-reclassified set (#1878). That is
            // written before the first row moves.
            let changed = changed
                .into_iter()
                .map(|path| path.as_ref().to_path_buf())
                .collect::<Vec<_>>();
            let normalizer = WatchPolicy::new(project_root);
            let requested = changed
                .iter()
                .filter_map(|path| normalizer.normalize_relative(path))
                .collect::<BTreeSet<_>>();
            crate::git_pending::extend_for_incremental(
                &store,
                project_root,
                &scope.options,
                &requested,
            )?;
            let outcome = sync_paths_with_store(
                &mut store,
                project_root,
                changed,
                None,
                &RehashUnder::default(),
                &scope,
                &include,
                &exclude,
                started,
                |_, _| {},
            )?;
            store.finish_current_mutation()?;
            Ok(outcome)
        }
        SyncWriter::MigrationRequired(authorization) => crate::migrate::migrate_project(
            project_root,
            &paths,
            authorization,
            &scope,
            started,
            |_, _| {},
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_paths_with_store(
    store: &mut Store,
    project_root: &Path,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    scanned: Option<&HashSet<String>>,
    rehash: &RehashUnder,
    scope: &ProjectScope,
    include: &[String],
    exclude: &[String],
    started: std::time::Instant,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    let policy = WatchPolicy::with_config(
        project_root,
        &scope.options.ignore_dirs,
        &scope.options.ignore_paths,
        include,
        exclude,
    )
    .with_extension_overrides(Arc::clone(&scope.options.extensions));
    let mut outcome = SyncOutcome::default();
    let mut changed = false;
    let mut seen = HashSet::new();

    let mut dependent_sites = BTreeMap::new();
    let mut dependent_fallbacks = BTreeSet::new();
    let mut reindexed = HashSet::new();
    let mut changed_names = ChangedNames::default();
    // An index without the module-surface marker was built before the rows
    // existed, or a sync failed after its first mutation: this sync
    // reconciles every JavaScript-family file, even with nothing changed.
    let surface_bootstrap = crate::module_surfaces::needs_bootstrap(store)?;
    // The same for the C/C++ dependents of changed type declarations: every
    // C/C++ file is re-resolved once (W4-CPP-04).
    let cpp_bootstrap = crate::cpp_dependents::needs_bootstrap(store)?;
    // And for the Java files whose resolution reads other files' heads and
    // packages: every Java file is re-resolved once (W4-JVM-01).
    let java_bootstrap = crate::java_heads::needs_bootstrap(store)?;
    let kotlin_bootstrap = crate::kotlin_surfaces::needs_bootstrap(store)?;
    let scala_bootstrap = crate::scala_surfaces::needs_bootstrap(store)?;
    let csharp_bootstrap = crate::csharp_surfaces::needs_bootstrap(store)?;
    let member_bootstrap = crate::member_surfaces::needs_bootstrap(store)?;
    let ruby_bootstrap = crate::ruby_heads::needs_bootstrap(store)?;
    let cfml_bootstrap = crate::cfml_heads::needs_bootstrap(store)?;
    let swift_bootstrap = crate::swift_heads::needs_bootstrap(store)?;
    let objc_bootstrap = crate::objc_heads::needs_bootstrap(store)?;
    let go_bootstrap = crate::go_heads::needs_bootstrap(store)?;
    // And for the Rust files writing a path receiver whose type came or
    // went: every Rust file is re-resolved once (W4-RS-03).
    let rust_bootstrap = crate::rust_receivers::needs_bootstrap(store)?;
    // And for the PHP files whose calls walk other files' class heads:
    // every PHP file is re-resolved once (W4-PHP-01).
    let php_bootstrap = crate::php_heads::needs_bootstrap(store)?;

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
        // A full sync reconciles against the scan it just ran, so the scan alone
        // decides scope, exactly as for `index --force`. The watch policy is
        // stricter on purpose (it never watches `.cache`, a top-level
        // `.codegraph-*` directory, and the other watch-only defaults), so it may
        // only judge the paths an incremental sync was handed.
        let in_scope = match scanned {
            Some(scanned) => scanned.contains(&relative),
            None => policy.should_handle_file(&relative),
        };
        if !in_scope && store.file_by_path(&relative)?.is_none() {
            outcome.files_ignored += 1;
            on_progress(done + 1, total);
            continue;
        }
        if sync_one(
            project_root,
            store,
            &relative,
            scope,
            rehash,
            &mut outcome,
            &mut dependent_sites,
            &mut dependent_fallbacks,
            &mut changed_names,
            !in_scope,
        )? {
            changed = true;
            reindexed.insert(relative);
        }
        on_progress(done + 1, total);
    }

    if changed
        || surface_bootstrap
        || cpp_bootstrap
        || java_bootstrap
        || kotlin_bootstrap
        || scala_bootstrap
        || csharp_bootstrap
        || member_bootstrap
        || ruby_bootstrap
        || cfml_bootstrap
        || swift_bootstrap
        || objc_bootstrap
        || go_bootstrap
        || rust_bootstrap
        || php_bootstrap
    {
        note_python_global_readers(
            project_root,
            store,
            scope,
            &reindexed,
            &mut changed_names,
            &mut dependent_sites,
            &mut dependent_fallbacks,
        )?;
        note_python_fixture_readers(
            project_root,
            store,
            &reindexed,
            &changed_names,
            &mut dependent_sites,
            &mut dependent_fallbacks,
        )?;
        // What other files read from a changed JavaScript-family module's
        // text no node records (W4a-F3): the files resolving through it, up
        // the barrels that forward it, re-resolve whole.
        let surfaces = crate::module_surfaces::note_surface_changes(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.js_surfaces,
            &reindexed,
            !changed_names.added_files.is_empty() || !changed_names.removed_files.is_empty(),
            surface_bootstrap,
        )?;
        for file in &surfaces.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(surfaces.names.iter().cloned());
        // What C/C++ resolution in an unchanged file read from a changed
        // file's text: its same-stem header, or the aliases and class heads
        // an alias lookup walks (W4-CPP-04), or the namespace aliases and
        // macros a qualified name reads (W4-CPP-07).
        let namespaces = crate::cpp_namespace_rows::namespace_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.cpp.namespaces,
            &reindexed,
            cpp_bootstrap,
        )?;
        for file in changed_names.cpp.dependent_files(
            store,
            project_root,
            scope.options.max_file_size,
            &reindexed,
            cpp_bootstrap,
            &namespaces.names,
        )? {
            dependent_sites.remove(&file);
            dependent_fallbacks.insert(file);
        }
        // What Java resolution in an unchanged file read from a changed
        // file: the heads its scope walks pass through, the packages its
        // imports are from (W4-JVM-01).
        let java = crate::java_heads::java_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.java,
            &reindexed,
            &changed_names.removed_files,
            java_bootstrap,
        )?;
        for file in &java.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        // And Kotlin's: the heads its walks pass through, the receivers its
        // lambdas run on, the project-wide receiver types and a Gradle
        // script's owners (W4-JVM-03).
        let kotlin = crate::kotlin_surfaces::kotlin_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.kotlin,
            &reindexed,
            &changed_names.removed_files,
            kotlin_bootstrap,
        )?;
        for file in &kotlin.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(kotlin.names.iter().cloned());
        // And Scala's: the heads and package objects its walks pass through,
        // and what a candidate's own text says of it (W4-SC-01).
        let scala = crate::scala_surfaces::scala_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.scala,
            &reindexed,
            &changed_names.removed_files,
            scala_bootstrap,
        )?;
        for file in &scala.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(scala.names.iter().cloned());
        // And C#'s: the `global using`s of a project's files, the heads a
        // nested type is reached through, and owners that flipped between a
        // type and a namespace (W4-CS-01).
        let csharp = crate::csharp_surfaces::csharp_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.csharp,
            &reindexed,
            &changed_names.removed_files,
            csharp_bootstrap,
        )?;
        for file in &csharp.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(csharp.names.iter().cloned());
        // And what a C#, Java or Kotlin call through a declared member reads
        // of the classes it inherits (W4-CS-03).
        let members = crate::member_surfaces::member_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.members,
            &reindexed,
            &changed_names.removed_files,
            member_bootstrap,
        )?;
        for file in &members.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        // And what a bare Ruby call reads of the classes and modules its
        // class inherits and mixes in (W4-RB-01).
        let ruby = crate::ruby_heads::ruby_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.ruby,
            &reindexed,
            &changed_names.removed_files,
            ruby_bootstrap,
        )?;
        for file in &ruby.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        // And what a bare CFML call in a component reads of the components
        // its `extends` chain reaches (W4-CF-01).
        let cfml = crate::cfml_heads::cfml_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.cfml,
            &reindexed,
            &changed_names.removed_files,
            cfml_bootstrap,
        )?;
        for file in &cfml.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        // And what a Swift type reference reads of the type declarations'
        // heads: extension or not, and the supertypes they name (W4-SW-01).
        let swift = crate::swift_heads::swift_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.swift,
            &reindexed,
            &changed_names.removed_files,
            swift_bootstrap,
        )?;
        for file in &swift.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(swift.names.iter().cloned());
        // And what a message to `self` or `super`, an untyped receiver and a
        // typed local read of the Objective-C classes' heads and properties'
        // declared types (W4-SW-04).
        let objc = crate::objc_heads::objc_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.objc,
            &reindexed,
            &changed_names.removed_files,
            objc_bootstrap,
        )?;
        for file in &objc.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(objc.names.iter().cloned());
        // And what a Go call reads of the type declarations its receiver's
        // type and fields reach: embeddings and field types (W4-GO-01).
        let go = crate::go_heads::go_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.go,
            &reindexed,
            &changed_names.removed_files,
            go_bootstrap,
        )?;
        for file in &go.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(go.names.iter().cloned());
        // And what a PHP call through `$this`, `self`, `static` or `parent`,
        // or through a receiver named after a class, reads of the ancestry's
        // heads: `extends` and trait `use`s (W4-PHP-01).
        let php = crate::php_heads::php_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.php,
            &reindexed,
            &changed_names.removed_files,
            php_bootstrap,
        )?;
        for file in &php.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for affected in &php.sites {
            merge_dependent_site(
                &mut dependent_sites,
                &mut dependent_fallbacks,
                affected.clone(),
            );
        }
        let moved_names: Vec<String> = php.moved_names.iter().cloned().collect();
        for affected in store.reference_sites_of_edges_to_named_targets(&moved_names)? {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        changed_names.note_all(php.names.iter().cloned());
        // A CamelCase path receiver is an outside type unless the project
        // declares a type, trait or alias of its name (upstream #2177): the
        // unchanged Rust files writing a name that came or went as a
        // receiver re-resolve the references written through it.
        let receiver_dependents = crate::rust_receivers::receiver_dependents(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.rust_receiver_types,
            &reindexed,
            &changed_names.removed_files,
        )?;
        if rust_bootstrap {
            for file in crate::rust_receivers::every_rust_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        // What a changed Python import statement re-exports moves for its
        // module's importers without any node of those names changing.
        if !changed_names.python_imports.is_empty() {
            let context = codegraph_resolve::StoreResolutionContext::new(
                store,
                project_root.to_string_lossy(),
            )
            .with_max_file_size(scope.options.max_file_size);
            for (file, statement) in &changed_names.python_imports {
                changed_names.any.extend(
                    codegraph_resolve::import_resolver::python_reexported_names(
                        file, statement, &context,
                    ),
                );
            }
        }
        let name_list: Vec<String> = changed_names.any.iter().cloned().collect();
        for affected in store.reference_sites_of_edges_to_named_targets(&name_list)? {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        let candidate_list: Vec<String> = changed_names.candidates.iter().cloned().collect();
        for affected in
            store.reference_sites_of_same_file_edges_to_named_targets(&candidate_list)?
        {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        // #2392 — an import names a file, a folder or a namespace, which the
        // name lookup never matches: the parked imports an added file or a
        // changed file's namespace can satisfy send their files down the
        // whole-file lane, so they resolve as a fresh index resolves them.
        let (path_keys, module_names) = changed_names.import_retry_keys();
        for parked in
            store.retryable_failed_imports(&path_keys, &module_names, IMPORT_RETRY_CEILING)?
        {
            if !reindexed.contains(&parked.file_path) {
                dependent_fallbacks.insert(parked.file_path);
            }
        }
        let mut resolver = ReferenceResolver::new(project_root.to_string_lossy())
            .with_max_file_size(scope.options.max_file_size);
        {
            let context = resolver.store_context(store);
            resolver.initialize(&context);
        }
        // Re-run framework per-file extract for reindexed files whose framework
        // nodes/refs were dropped on re-extraction (upstream tree-sitter.ts:4796-4819).
        if resolver.has_framework_resolvers() {
            let reindexed_files: Vec<String> = reindexed.iter().cloned().collect();
            resolver.extract_and_persist_frameworks_with(
                store,
                &reindexed_files,
                &scope.framework,
                &scope.options.extensions,
            )?;
        }
        let (scope_sites, fallback_files) = refresh_dependent_refs(
            project_root,
            store,
            scope,
            &resolver,
            &dependent_sites,
            &dependent_fallbacks,
            &receiver_dependents,
            &reindexed,
        )?;
        let mut scope_files = reindexed.clone();
        scope_files.extend(fallback_files);
        #[cfg(test)]
        if tests::FAIL_BEFORE_RESOLUTION.with(std::cell::Cell::get) {
            bail!("injected sync failure before resolution");
        }
        resolver.resolve_incremental_and_persist(
            store,
            &scope_files,
            &scope_sites,
            &changed_names.any,
        )?;
        // Cross-file framework finalization on every sync (upstream index.ts:464).
        resolver.run_post_extract(store)?;
        // Last: the surface and namespace rows, then the markers the first
        // mutation withdrew.
        surfaces.write_rows(store)?;
        namespaces.write_rows(store)?;
        java.write_rows(store)?;
        kotlin.write_rows(store)?;
        scala.write_rows(store)?;
        csharp.write_rows(store)?;
        members.write_rows(store)?;
        ruby.write_rows(store)?;
        cfml.write_rows(store)?;
        swift.write_rows(store)?;
        objc.write_rows(store)?;
        go.write_rows(store)?;
        php.write_rows(store)?;
        crate::module_surfaces::finish_bootstrap(store)?;
        crate::cpp_dependents::write_marker(store)?;
        crate::java_heads::write_marker(store)?;
        crate::kotlin_surfaces::write_marker(store)?;
        crate::scala_surfaces::write_marker(store)?;
        crate::csharp_surfaces::write_marker(store)?;
        crate::member_surfaces::write_marker(store)?;
        crate::ruby_heads::write_marker(store)?;
        crate::cfml_heads::write_marker(store)?;
        crate::swift_heads::write_marker(store)?;
        crate::objc_heads::write_marker(store)?;
        crate::go_heads::write_marker(store)?;
        crate::rust_receivers::write_marker(store)?;
        crate::php_heads::write_marker(store)?;
    }

    // Orphan sweep (#1187, upstream index.ts:645-682): a full-index resolution
    // pass killed mid-run leaves the refs it never reached parked in
    // unresolved_refs and sets the incomplete-resolution marker. The scoped
    // incremental pass above only revisits changed files' refs, so those orphaned
    // call edges stay missing until a full re-index. When the marker is present —
    // INCLUDING on a no-change sync — grind the whole table down with the batched
    // resolver, which heals the wedged index and clears the marker on success. A
    // healthy index (marker absent) skips this entirely, so an ordinary sync is
    // byte-for-byte unchanged.
    if store.is_resolution_incomplete()? {
        let stats = sweep_orphaned_refs(project_root, store, scope.options.max_file_size)?;
        outcome.pending_refs_processed = stats.total;
        outcome.pending_refs_resolved = stats.resolved;
        outcome.pending_refs_unresolved = stats.unresolved;
    }

    outcome.duration_ms = started.elapsed().as_millis();
    let mut changed_paths: Vec<String> = reindexed.into_iter().collect();
    changed_paths.sort();
    outcome.changed_paths = changed_paths;
    Ok(outcome)
}

/// Rebuild only the affected source-site groups of unchanged dependent files,
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
/// memory only. Base and framework refs are merged, then every captured site is
/// refreshed as one indivisible sibling group; unrelated edges and unresolved row
/// ids stay untouched. A NULL historical edge coordinate or a captured site with
/// no fresh ref upgrades that file to the correctness-first whole-file fallback,
/// which still preserves nodes, `contains` edges, and incoming edges. The returned
/// site lane plus fallback-file lane are resolved against the final graph.
#[allow(clippy::too_many_arguments)]
fn refresh_dependent_refs(
    project_root: &Path,
    store: &mut Store,
    scope: &ProjectScope,
    resolver: &ReferenceResolver,
    dependent_sites: &BTreeMap<String, BTreeSet<ReferenceSite>>,
    fallback_files: &BTreeSet<String>,
    receiver_names: &BTreeMap<String, BTreeSet<String>>,
    already_reindexed: &HashSet<String>,
) -> Result<(BTreeSet<ReferenceSite>, HashSet<String>)> {
    let dependents = dependent_sites
        .keys()
        .chain(fallback_files)
        .chain(receiver_names.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut refreshed_sites = BTreeSet::new();
    let mut refreshed_files = HashSet::new();
    for relative in dependents {
        if already_reindexed.contains(&relative) {
            continue;
        }
        if !project_root.join(&relative).exists() {
            continue;
        }
        let refs = extract_dependent_refs(project_root, &relative, scope, resolver)?;
        let sites = dependent_sites.get(&relative);
        let needs_fallback = fallback_files.contains(&relative)
            || sites.is_some_and(|sites| {
                sites.iter().any(|site| {
                    !refs
                        .iter()
                        .any(|reference| reference_matches_site(reference, site))
                })
            });
        if needs_fallback {
            store.replace_resolution_state_from_file(&relative, &refs)?;
            refreshed_files.insert(relative);
            continue;
        }
        // The sites of the references written through a Rust receiver whose
        // type came or went (W4-RS-03) join the file's dependent sites.
        let mut site_set: BTreeSet<ReferenceSite> = sites.cloned().unwrap_or_default();
        if let Some(names) = receiver_names.get(&relative) {
            site_set.extend(
                refs.iter()
                    .filter(|reference| {
                        crate::rust_receivers::is_written_through(&reference.reference_name, names)
                    })
                    .map(|reference| ReferenceSite {
                        from_node_id: reference.from_node_id.clone(),
                        line: reference.line,
                        col: reference.col,
                    }),
            );
        }
        if site_set.is_empty() {
            continue;
        }
        let selected = refs
            .into_iter()
            .filter(|reference| {
                site_set
                    .iter()
                    .any(|site| reference_matches_site(reference, site))
            })
            .collect::<Vec<_>>();
        let site_list = site_set.iter().cloned().collect::<Vec<_>>();
        store.replace_resolution_state_at_sites(&site_list, &selected)?;
        refreshed_sites.extend(site_set);
    }
    Ok((refreshed_sites, refreshed_files))
}

fn extract_dependent_refs(
    project_root: &Path,
    relative: &str,
    scope: &ProjectScope,
    resolver: &ReferenceResolver,
) -> Result<Vec<codegraph_core::types::UnresolvedRef>> {
    let result = extract_file_with_options(project_root, relative, &scope.options)?;
    let framework = resolver.collect_framework_extraction_with(
        &[relative.to_string()],
        &scope.framework,
        &scope.options.extensions,
    );
    let node_ids = result
        .nodes
        .iter()
        .chain(&framework.nodes)
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let mut refs = result.unresolved_references;
    refs.extend(framework.unresolved_references);
    refs.retain(|reference| node_ids.contains(reference.from_node_id.as_str()));
    Ok(refs)
}

fn reference_matches_site(
    reference: &codegraph_core::types::UnresolvedRef,
    site: &ReferenceSite,
) -> bool {
    reference.from_node_id == site.from_node_id
        && reference.line == site.line
        && reference.col == site.col
}

fn merge_dependent_site(
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    fallback_files: &mut BTreeSet<String>,
    affected: FileReferenceSite,
) {
    match affected.site {
        Some(site) if !fallback_files.contains(&affected.file_path) => {
            dependent_sites
                .entry(affected.file_path)
                .or_default()
                .insert(site);
        }
        Some(_) => {}
        None => {
            dependent_sites.remove(&affected.file_path);
            fallback_files.insert(affected.file_path);
        }
    }
}

/// Grind the whole `unresolved_refs` table with the batched resolver to heal an
/// index whose full-resolution pass was interrupted (#1187 marker set). The
/// batched pass re-arms and clears the marker itself, so on success the index is
/// no longer flagged partial. Framework per-file extract is re-run first so any
/// framework refs an interrupted run never re-injected are present for the sweep.
fn sweep_orphaned_refs(
    project_root: &Path,
    store: &mut Store,
    max_file_size: u64,
) -> Result<codegraph_resolve::ResolutionStats> {
    let mut resolver =
        ReferenceResolver::new(project_root.to_string_lossy()).with_max_file_size(max_file_size);
    {
        let context = resolver.store_context(store);
        resolver.initialize(&context);
    }
    // Framework extraction must not run in the sweep. The marker is set only
    // inside the batched resolver, which runs AFTER a full index has already
    // persisted its framework `unresolved_refs`, so those rows are present in the
    // DB by the time the marker exists. Re-extracting them would re-inject
    // duplicate rows (`unresolved_refs` has no UNIQUE constraint), which
    // accumulate across interrupt→heal cycles and diverge from `index --force`.
    // Re-resolving the existing rows alone heals the interrupted pass byte-equal.
    let result = resolver.resolve_and_persist_batched(store, ORPHAN_SWEEP_BATCH_ROWS)?;
    resolver.run_post_extract(store)?;
    Ok(result.stats)
}

/// Batch size for the #1187 orphan sweep, matching the CLI full-index batch
/// (`RESOLVE_BATCH_ROWS`) so a healed index is byte-equal to `index --force`.
const ORPHAN_SWEEP_BATCH_ROWS: usize = 5_000;

#[cfg(test)]
pub(crate) fn default_db_path(project_root: &Path) -> Result<PathBuf> {
    Ok(codegraph_core::IndexPaths::resolve(
        project_root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?
    .current_db())
}

#[allow(clippy::too_many_arguments)]
fn sync_one(
    project_root: &Path,
    store: &mut Store,
    relative: &str,
    scope: &ProjectScope,
    rehash: &RehashUnder,
    outcome: &mut SyncOutcome,
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    dependent_fallbacks: &mut BTreeSet<String>,
    changed_names: &mut ChangedNames,
    force_absent: bool,
) -> Result<bool> {
    let full = project_root.join(relative);
    // One stat serves as both the existence check and the (mtime, size) source.
    let metadata = match (!force_absent).then(|| fs::metadata(&full)) {
        Some(Ok(metadata)) if metadata.is_file() => metadata,
        _ => {
            return remove_tracked_file(
                store,
                relative,
                outcome,
                dependent_sites,
                dependent_fallbacks,
                changed_names,
            );
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
    if let Some(file) = &stored
        && file.size == metadata.len() as i64
        && file.modified_at == modified_millis(&metadata)
        && !rehash.covers(relative)
    {
        outcome.files_skipped_unchanged += 1;
        return Ok(false);
    }

    let (metadata, source) = read_source_file(&full, relative, scope.options.max_file_size)
        .with_context(|| format!("read {}", full.display()))?;
    // A file that became a video clip named `.ts` is no longer source: it leaves
    // the index like a deletion, and an untracked one is ignored (#1910).
    let Some(hash_input) = source.hash_input() else {
        return remove_tracked_file(
            store,
            relative,
            outcome,
            dependent_sites,
            dependent_fallbacks,
            changed_names,
        );
    };
    let content_hash = hash_content(&hash_input);
    let added = stored.is_none();
    let c_family_before = stored
        .as_ref()
        .is_some_and(|file| crate::cpp_dependents::is_c_family(file.language));
    let namespace_row_before = stored
        .as_ref()
        .is_some_and(|file| crate::cpp_namespace_rows::is_row_file(relative, file.language));
    // Authoritative content gate, port of the upstream hash gate in
    // `upstream extraction/index.ts:1326-1337,1465-1483`.
    if stored.is_some_and(|file| file.content_hash == content_hash) {
        outcome.files_skipped_unchanged += 1;
        return Ok(false);
    }

    changed_names.begin_mutation(store)?;
    let mut importers = BTreeSet::new();
    for dependent in store.reference_sites_dependent_on_file(relative)? {
        importers.insert(dependent.file_path.clone());
        merge_dependent_site(dependent_sites, dependent_fallbacks, dependent);
    }
    let language_after = detect_language_of(relative, &source, &scope.options.extensions);
    let c_family_after = crate::cpp_dependents::is_c_family(language_after);
    let c_family = c_family_after || c_family_before;
    changed_names.cpp.before_change(store, relative, c_family)?;
    // What the namespace tables read of the file's text (W4-CPP-07).
    let namespace_row =
        namespace_row_before || crate::cpp_namespace_rows::is_row_file(relative, language_after);
    if namespace_row {
        changed_names
            .cpp
            .namespaces
            .before_change(store, relative)?;
    }
    reextract_into_store(
        store,
        relative,
        scope,
        changed_names,
        (&metadata, &source, content_hash),
    )?;
    changed_names
        .cpp
        .after_change(store, relative, c_family, c_family_after)?;
    if namespace_row {
        changed_names.cpp.namespaces.after_change(
            relative,
            crate::cpp_namespace_rows::scan_of(relative, language_after, &source),
        );
    }
    if let Some(change) = changed_names.js_surfaces.get_mut(relative) {
        change.importers.extend(importers.iter().cloned());
        change.added = added;
    }
    if let Some(change) = changed_names.python_files.get_mut(relative) {
        change.importers.extend(importers);
    }
    if added {
        changed_names.added_files.insert(relative.to_string());
    }
    outcome.files_reindexed += 1;
    Ok(true)
}

/// Drop `relative`'s record when it is tracked, collecting what its removal
/// invalidates; count it as ignored otherwise.
fn remove_tracked_file(
    store: &mut Store,
    relative: &str,
    outcome: &mut SyncOutcome,
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    dependent_fallbacks: &mut BTreeSet<String>,
    changed_names: &mut ChangedNames,
) -> Result<bool> {
    let Some(stored) = store.file_by_path(relative)? else {
        outcome.files_ignored += 1;
        return Ok(false);
    };
    changed_names.begin_mutation(store)?;
    let mut importers = BTreeSet::new();
    for dependent in store.reference_sites_dependent_on_file(relative)? {
        importers.insert(dependent.file_path.clone());
        merge_dependent_site(dependent_sites, dependent_fallbacks, dependent);
    }
    let c_family = crate::cpp_dependents::is_c_family(stored.language);
    changed_names.cpp.before_change(store, relative, c_family)?;
    if crate::cpp_namespace_rows::is_row_file(relative, stored.language) {
        changed_names
            .cpp
            .namespaces
            .before_change(store, relative)?;
        changed_names.cpp.namespaces.after_change(relative, None);
    }
    changed_names.note_removed(node_names_in_file(store, relative)?);
    let old_nodes = store.nodes_by_file_path(relative)?;
    changed_names.rust_receiver_types.extend(
        old_nodes
            .iter()
            .filter(|node| crate::rust_receivers::is_receiver_type(node))
            .map(|node| node.name.clone()),
    );
    changed_names.removed_files.insert(relative.to_string());
    if codegraph_resolve::module_surface::has_module_surface(relative, stored.language) {
        let change = changed_names
            .js_surfaces
            .entry(relative.to_string())
            .or_default();
        change.old = crate::module_surfaces::old_row(store, relative)?;
        change.new = None;
        change.removed = true;
        change.importers.extend(importers.iter().cloned());
        change.note_nodes(&old_nodes);
    }
    let statements = python_import_statements(&old_nodes);
    changed_names.python_imports.extend(
        statements
            .iter()
            .map(|statement| (relative.to_string(), statement.clone())),
    );
    if old_nodes
        .iter()
        .any(|node| node.language == Language::Python)
    {
        let change = changed_names
            .python_files
            .entry(relative.to_string())
            .or_default();
        change.old_globals.extend(python_module_globals(&old_nodes));
        change.statements.extend(statements);
        change.importers.extend(importers);
        note_python_fixture_holders(change, &old_nodes);
    }
    changed_names.java.note(store, relative, &old_nodes, None)?;
    changed_names
        .kotlin
        .note(store, relative, &old_nodes, None)?;
    changed_names
        .scala
        .note(store, relative, &old_nodes, None)?;
    changed_names
        .csharp
        .note(store, relative, &old_nodes, None)?;
    changed_names.members.note(store, relative, None)?;
    changed_names.ruby.note(store, relative, None)?;
    changed_names.cfml.note(store, relative, &old_nodes, None)?;
    changed_names
        .swift
        .note(store, relative, &old_nodes, None)?;
    changed_names.objc.note(store, relative, None)?;
    changed_names.go.note(store, relative, None)?;
    changed_names.php.note(store, relative, &old_nodes, None)?;
    delete_unresolved_refs_by_file(store, relative)?;
    store.delete_file_record(relative)?;
    changed_names
        .cpp
        .after_change(store, relative, c_family, false)?;
    outcome.files_removed += 1;
    Ok(true)
}

/// The text of every Python import statement among `nodes`.
fn python_import_statements(nodes: &[Node]) -> BTreeSet<String> {
    nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Import && node.language == Language::Python)
        .filter_map(|node| node.signature.clone())
        .collect()
}

/// The names of the Python module globals among `nodes`: variables and
/// constants no class or function of the file holds.
fn python_module_globals(nodes: &[Node]) -> BTreeSet<String> {
    nodes
        .iter()
        .filter(|node| {
            node.language == Language::Python
                && matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
                && !nodes.iter().any(|scope| {
                    matches!(
                        scope.kind,
                        NodeKind::Class | NodeKind::Function | NodeKind::Method
                    ) && scope.start_line <= node.start_line
                        && scope.end_line >= node.start_line
                })
        })
        .map(|node| node.name.clone())
        .collect()
}

/// What `nodes` held of a file's pytest fixtures before a change: its
/// top-level functions and its `pytest_plugins` value.
fn note_python_fixture_holders(change: &mut PythonFileChange, nodes: &[Node]) {
    for node in nodes {
        if node.language != Language::Python || node.qualified_name.contains("::") {
            continue;
        }
        if node.kind == NodeKind::Function {
            change.old_functions.insert(node.name.clone());
        }
        if matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
            && node.name == "pytest_plugins"
        {
            change.old_plugins.extend(node.signature.clone());
        }
    }
}

/// The top-level functions of the Python file `file` in the store.
fn python_top_level_functions(store: &Store, file: &str) -> Result<BTreeSet<String>> {
    Ok(store
        .nodes_by_file_path(file)?
        .into_iter()
        .filter(|node| {
            node.language == Language::Python
                && node.kind == NodeKind::Function
                && !node.qualified_name.contains("::")
        })
        .map(|node| node.name)
        .collect())
}

/// Pytest fixture parameters (upstream #2228): a test's parameter is the
/// class its in-reach fixture returns, read from the fixture's body and from
/// the conftests' plugin lists, which no node records. A changed
/// `conftest.py` reaches the tests under its directory with its own
/// functions and those of the modules it pulls in, before or after the
/// change; a changed module a conftest pulls in reaches the tests under that
/// conftest. Every function there with a parameter named like one of those
/// functions has all its reference sites refreshed.
fn note_python_fixture_readers(
    project_root: &Path,
    store: &Store,
    reindexed: &HashSet<String>,
    changed_names: &ChangedNames,
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    dependent_fallbacks: &mut BTreeSet<String>,
) -> Result<()> {
    use codegraph_resolve::python_globals::{
        conftest_plugged_modules, parameter_names, quoted_module_files,
    };
    let is_conftest = |file: &str| file == "conftest.py" || file.ends_with("/conftest.py");
    let dir_of = |file: &str| {
        file.rsplit_once('/')
            .map_or(String::new(), |(dir, _)| dir.to_string())
    };
    let names_module =
        |file: &str, module: &str| file == module || file.ends_with(&format!("/{module}"));
    // (directory the fixtures reach, the fixture names).
    let mut reaches: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut files: Option<Vec<String>> = None;
    let mut conftests: Option<Vec<(String, Vec<String>)>> = None;
    for (file, change) in &changed_names.python_files {
        let mut names: BTreeSet<String> = change.old_functions.clone();
        names.extend(python_top_level_functions(store, file)?);
        if is_conftest(file) {
            let mut plugged = BTreeSet::new();
            for statement in &change.statements {
                plugged.extend(conftest_plugged_modules(statement));
            }
            for value in &change.old_plugins {
                plugged.extend(quoted_module_files(value));
            }
            if let Ok(text) = fs::read_to_string(project_root.join(file)) {
                plugged.extend(conftest_plugged_modules(&text));
            }
            if !plugged.is_empty() {
                let all = match &files {
                    Some(all) => all,
                    None => files.insert(
                        store
                            .all_files()?
                            .into_iter()
                            .map(|record| record.path)
                            .collect(),
                    ),
                };
                for path in all {
                    if plugged.iter().any(|module| names_module(path, module)) {
                        names.extend(python_top_level_functions(store, path)?);
                    }
                }
            }
            if !names.is_empty() {
                reaches.push((dir_of(file), names));
            }
            continue;
        }
        // Resolution takes a decorator spelled `fixture` (`@pytest.fixture`,
        // `@fixture` imported from anywhere) as a fixture, never asking what
        // the module imports, so every changed module a conftest pulls in is
        // followed.
        if names.is_empty() {
            continue;
        }
        let pluggers = match &conftests {
            Some(pluggers) => pluggers,
            None => {
                let all = match &files {
                    Some(all) => all,
                    None => files.insert(
                        store
                            .all_files()?
                            .into_iter()
                            .map(|record| record.path)
                            .collect(),
                    ),
                };
                let found = all
                    .iter()
                    .filter(|path| is_conftest(path))
                    .map(|path| {
                        let plugged = fs::read_to_string(project_root.join(path))
                            .map(|text| conftest_plugged_modules(&text))
                            .unwrap_or_default();
                        (path.clone(), plugged)
                    })
                    .collect();
                conftests.insert(found)
            }
        };
        for (conftest, plugged) in pluggers {
            if plugged.iter().any(|module| names_module(file, module)) {
                reaches.push((dir_of(conftest), names.clone()));
            }
        }
    }
    if reaches.is_empty() {
        return Ok(());
    }
    let readers: Vec<String> = store
        .python_callable_signatures()?
        .into_iter()
        .filter(|(_, file, signature)| {
            if reindexed.contains(file) {
                return false;
            }
            let parameters = parameter_names(signature);
            reaches.iter().any(|(dir, names)| {
                (dir.is_empty() || file.starts_with(&format!("{dir}/")))
                    && parameters.iter().any(|name| names.contains(name))
            })
        })
        .map(|(id, _, _)| id)
        .collect();
    for found in store.reference_sites_from_nodes(&readers)? {
        merge_dependent_site(dependent_sites, dependent_fallbacks, found);
    }
    Ok(())
}

/// Method values that read through a Python module global (upstream #2291
/// a): its type is read from its module's assignments and from other
/// modules' writes, which no node records. For every changed Python file,
/// the modules it declares or can write a global of are read: the
/// unresolved method values through a global the type can now come from are
/// retried by name, and the resolved ones through any global it had are
/// refreshed by site, in the module and in its importers.
fn note_python_global_readers(
    project_root: &Path,
    store: &Store,
    scope: &ProjectScope,
    reindexed: &HashSet<String>,
    changed_names: &mut ChangedNames,
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    dependent_fallbacks: &mut BTreeSet<String>,
) -> Result<()> {
    use codegraph_resolve::python_globals::{
        imported_module_files, module_globals, names_through_global, reads_through_global,
        typable_module_globals,
    };
    if changed_names.python_files.is_empty() {
        return Ok(());
    }
    let context =
        codegraph_resolve::StoreResolutionContext::new(store, project_root.to_string_lossy())
            .with_max_file_size(scope.options.max_file_size);
    let mut names = BTreeSet::new();
    let mut sites = Vec::new();
    let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (file, change) in &changed_names.python_files {
        let mut modules =
            imported_module_files(file, change.statements.iter().map(String::as_str), &context);
        modules.insert(file.clone());
        for module in modules {
            let typable = typable_module_globals(&module, &context);
            let mut globals = typable.clone();
            let mut readers = BTreeSet::new();
            if module == *file {
                globals.extend(module_globals(&module, &context));
                globals.extend(change.old_globals.iter().cloned());
                readers.extend(change.importers.iter().cloned());
            } else {
                readers.extend(
                    store
                        .reference_sites_dependent_on_file(&module)?
                        .into_iter()
                        .map(|site| site.file_path),
                );
            }
            if globals.is_empty() {
                continue;
            }
            readers.insert(module.clone());
            let readers: Vec<String> = readers
                .into_iter()
                .filter(|reader| !reindexed.contains(reader))
                .collect();
            if readers.is_empty() {
                continue;
            }
            if !typable.is_empty() {
                for reference in store.unresolved_refs_by_files(&readers)? {
                    if reference.is_function_ref
                        && reference.language == Language::Python
                        && names_through_global(&reference.reference_name, &typable)
                    {
                        names.insert(reference.reference_name);
                    }
                }
            }
            for found in store.function_ref_sites_in_files(&readers)? {
                let Some(site) = &found.site else {
                    sites.push(found);
                    continue;
                };
                let source = sources.entry(found.file_path.clone()).or_insert_with(|| {
                    fs::read_to_string(project_root.join(&found.file_path)).ok()
                });
                let before = source.as_deref().and_then(|source| {
                    let line = source
                        .lines()
                        .nth(usize::try_from(site.line).ok()?.checked_sub(1)?)?;
                    line.get(..usize::try_from(site.col).ok()?)
                });
                if before.is_none_or(|before| reads_through_global(before, &globals)) {
                    sites.push(found);
                }
            }
        }
    }
    changed_names.any.extend(names);
    for found in sites {
        if !reindexed.contains(&found.file_path) {
            merge_dependent_site(dependent_sites, dependent_fallbacks, found);
        }
    }
    Ok(())
}

/// Re-extract `relative` from the bytes `sync_one` already read and hashed, so
/// the record, its hash and its symbols all describe one read of the file.
fn reextract_into_store(
    store: &mut Store,
    relative: &str,
    scope: &ProjectScope,
    changed_names: &mut ChangedNames,
    (metadata, source, content_hash): (&fs::Metadata, &SourceText, String),
) -> Result<()> {
    let result = codegraph_extract::engine::extraction_of(relative, source, &scope.options, |_| {});
    let hash_input = source.hash_input().unwrap_or_default();
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
        content_hash,
        language: detect_language_of(relative, source, &scope.options.extensions),
        size: metadata.len() as i64,
        modified_at: modified_millis(metadata),
        indexed_at: now_millis(),
        node_count: result
            .nodes
            .iter()
            .filter(|node| node.file_path == relative)
            .count() as i64,
        errors: result.errors,
        generated: detect_generated_file(relative, &hash_input),
    };

    // A name's resolution outcomes (confidence, chosen target) depend on the set
    // of nodes carrying that name. A node id present before but not after, or
    // vice versa, touches its name; so does a node that keeps its id but changes
    // anything else resolution reads, such as an added `export`
    // (`ChangedNames::note_candidates`). Editing the tail of a file (no line
    // shift for earlier symbols) therefore contributes no names, keeping the
    // re-resolve scope minimal.
    //
    // A PHP node whose id changed only because it moved is the exception
    // (W4-PHP-01): every fact another file's resolution reads of a PHP
    // declaration is in its node or in the `php_heads` rows, which the PHP
    // lane compares, so it changes nothing elsewhere. The edges into this file
    // re-resolve through `reference_sites_dependent_on_file` above, and a
    // reference in another file reads this file's candidates' positions only
    // to order them among themselves (`order_candidates`): its id touches its
    // name only when the order of that name's nodes in this file changed too
    // (upstream likewise pairs a re-indexed file's old and new nodes by
    // identity and keeps the edges whose target only moved,
    // `pairReindexedNodes`). Other languages read declaration text no node
    // records (a C function's `static`), so their moved nodes still touch.
    let old_nodes = store.nodes_by_file_path(relative)?;
    let old_ids: HashSet<(&str, &str)> = old_nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let new_ids: HashSet<(&str, &str)> = result
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let order_before = positioned_candidate_keys(&old_nodes);
    let order_after = positioned_candidate_keys(&result.nodes);
    let not_only_php: HashSet<&str> = old_nodes
        .iter()
        .chain(&result.nodes)
        .filter(|node| node.language != Language::Php)
        .map(|node| node.name.as_str())
        .collect();
    for (_, name) in old_ids.symmetric_difference(&new_ids) {
        if not_only_php.contains(*name) || order_before.get(*name) != order_after.get(*name) {
            changed_names.any.insert((*name).to_string());
        }
    }
    changed_names.note_candidates(&old_nodes, &result.nodes);
    // What other files' Java resolution reads of this one's text and
    // packages, before and after (W4-JVM-01).
    changed_names.java.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.kotlin.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.scala.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.csharp.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.members.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.ruby.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.cfml.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.swift.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.objc.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.go.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.php.note(
        store,
        relative,
        &old_nodes,
        Some((file.language, source, &result.nodes)),
    )?;
    // No name in another file resolves into a minified script (upstream
    // #2204), and that verdict is read from the script's text: a script that
    // was or is generated can flip it with every node where it was, so all of
    // its names are touched and the refs naming them resolve afresh.
    let stored = store.file_by_path(relative)?;
    let was_generated = stored.as_ref().is_some_and(|stored| stored.generated);
    // A JavaScript-family module's surface is read from its text, which is
    // gone after this (W4a-F3): keep the row it had and the surface it has.
    if codegraph_resolve::module_surface::has_module_surface(relative, file.language)
        || stored.as_ref().is_some_and(|stored| {
            codegraph_resolve::module_surface::has_module_surface(relative, stored.language)
        })
    {
        let change = changed_names
            .js_surfaces
            .entry(relative.to_string())
            .or_default();
        change.old = crate::module_surfaces::old_row(store, relative)?;
        change.new = crate::module_surfaces::surface_of(relative, file.language, source);
        change.note_nodes(old_nodes.iter().chain(&result.nodes));
    }
    if is_minifiable_script(relative) && (was_generated || file.generated) {
        changed_names.note_all(
            old_nodes
                .iter()
                .chain(&result.nodes)
                .map(|node| node.name.clone()),
        );
    }
    // A C# extension method is reached through the `this` type and the
    // namespace its declaration reads (W3-05c), which no node records: a file
    // that declares one touches its method names, so the calls naming them
    // resolve afresh.
    if file.language == Language::CSharp && declares_csharp_extension_method(&hash_input) {
        changed_names.note_all(
            old_nodes
                .iter()
                .chain(&result.nodes)
                .filter(|node| node.kind == NodeKind::Method)
                .map(|node| node.name.clone()),
        );
    }
    for node in &result.nodes {
        if matches!(node.kind, NodeKind::Namespace | NodeKind::Module) {
            changed_names
                .module_names
                .insert(node.qualified_name.clone());
            changed_names.module_names.insert(node.name.clone());
        }
    }
    let (before, after) = (
        python_import_statements(&old_nodes),
        python_import_statements(&result.nodes),
    );
    changed_names.python_imports.extend(
        before
            .symmetric_difference(&after)
            .map(|statement| (relative.to_string(), statement.clone())),
    );
    if file.language == Language::Python
        || old_nodes
            .iter()
            .any(|node| node.language == Language::Python)
    {
        let change = changed_names
            .python_files
            .entry(relative.to_string())
            .or_default();
        change.old_globals.extend(python_module_globals(&old_nodes));
        change.statements.extend(before.union(&after).cloned());
        note_python_fixture_holders(change, &old_nodes);
    }

    delete_unresolved_refs_by_file(store, relative)?;
    store.delete_file_record(relative)?;
    store.upsert_nodes(&result.nodes)?;
    store.insert_edges(&result.edges)?;
    store.insert_unresolved_refs(&refs)?;
    store.upsert_file(&file)?;
    Ok(())
}

/// The names a sync's changed files touched, which the resolution of refs in
/// the files it did not reindex depends on.
#[derive(Debug, Default)]
struct ChangedNames {
    /// Every touched name: a node of it arrived, left, changed id (a move
    /// included), or changed anything else resolution reads. Cross-file edges to
    /// such a name are re-resolved, and so are the unresolved refs naming it.
    any: HashSet<String>,
    /// The touched names whose candidates changed as a ref outside their file
    /// sees them: a node arrived, left, or changed anything but its position.
    /// Only these can move an edge whose source and target share an unchanged
    /// file, so only these re-resolve such edges; a line shift elsewhere does not.
    candidates: HashSet<String>,
    /// The files this sync added: a parked import may name one (#2392).
    added_files: BTreeSet<String>,
    /// The namespaces and modules the changed files declare, which a parked
    /// import may name whole (`using Foo.Bar`, `import com.z.*`).
    module_names: HashSet<String>,
    /// The Python import statements that came or went, by file: a package
    /// re-exports what they bind (upstream #2247), so the names they can
    /// carry are touched once the store holds the new files.
    python_imports: BTreeSet<(String, String)>,
    /// What each changed Python file held before the change, for the module
    /// globals it declares or can write (upstream #2291 a).
    python_files: BTreeMap<String, PythonFileChange>,
    /// What each changed JavaScript-family file's module surface was and is
    /// (W4a-F3).
    js_surfaces: BTreeMap<String, crate::module_surfaces::SurfaceChange>,
    /// The files this sync removed.
    removed_files: BTreeSet<String>,
    /// Whether this sync already withdrew the surface version marker, as it
    /// does before its first mutation and restores once everything persisted.
    surface_dirty: bool,
    /// What the changed C/C++ files held that resolution in other files reads
    /// (W4-CPP-04).
    cpp: crate::cpp_dependents::CppChanges,
    /// What the changed Java, Kotlin and Scala files held that Java
    /// resolution in other files reads: heads and packages (W4-JVM-01).
    java: crate::java_heads::JavaChanges,
    /// What the changed files held that Kotlin resolution in other files
    /// reads: surfaces and class names (W4-JVM-03).
    kotlin: crate::kotlin_surfaces::KotlinChanges,
    /// What the changed files held that Scala resolution in other files
    /// reads: heads, package objects and text facts (W4-SC-01).
    scala: crate::scala_surfaces::ScalaChanges,
    /// What the changed files held that C# resolution in other files reads:
    /// `global using`s, heads and owners (W4-CS-01).
    csharp: crate::csharp_surfaces::CsharpChanges,
    /// What the changed files held that member-type inference in other
    /// files reads: class heads and member lines (W4-CS-03).
    members: crate::member_surfaces::MemberChanges,
    /// What the changed files held that a bare Ruby call in other files
    /// reads: class and module heads and mixins (W4-RB-01).
    ruby: crate::ruby_heads::RubyChanges,
    /// What the changed files held that a bare CFML call in other files
    /// reads: component classes and `extends` (W4-CF-01).
    cfml: crate::cfml_heads::CfmlChanges,
    swift: crate::swift_heads::SwiftChanges,
    /// What the changed files held that an Objective-C message in other
    /// files reads: class heads and property types (W4-SW-04).
    objc: crate::objc_heads::ObjcChanges,
    /// What the changed files held that a Go call in other files reads:
    /// type declarations' embeddings and field types (W4-GO-01).
    go: crate::go_heads::GoChanges,
    /// The Rust type, trait and alias names that came, went or changed: a
    /// CamelCase path receiver of such a name is the project's or an outside
    /// type by it (W4-RS-03).
    rust_receiver_types: BTreeSet<String>,
    /// What the changed files held that a PHP call in other files reads:
    /// class, trait, interface and enum heads (W4-PHP-01).
    php: crate::php_heads::PhpChanges,
}

/// What a changed Python file held before the change. A module global's
/// type is read from its module's assignments and other modules' writes,
/// which no node records, so the method values reading through it are
/// re-resolved from here.
#[derive(Debug, Default)]
struct PythonFileChange {
    /// Its module globals before the change.
    old_globals: BTreeSet<String>,
    /// Its import statements before and after: the modules whose globals it
    /// can write.
    statements: BTreeSet<String>,
    /// The files whose resolved references reached into it before the
    /// change (they are gone with its nodes by the time sync reads them).
    importers: BTreeSet<String>,
    /// Its top-level functions before the change: the pytest fixtures it
    /// could define.
    old_functions: BTreeSet<String>,
    /// What its `pytest_plugins` held before the change.
    old_plugins: Vec<String>,
}

/// How many parked imports one retry key may wake (upstream's per-name ceiling).
const IMPORT_RETRY_CEILING: usize = 500;

impl ChangedNames {
    /// Withdraw the module-surface version marker before this sync's first
    /// mutation: a sync that fails after it leaves the marker absent, and the
    /// next sync reconciles every JavaScript-family file (W4a-F3).
    fn begin_mutation(&mut self, store: &Store) -> Result<()> {
        if !self.surface_dirty {
            store.delete_project_metadata(codegraph_resolve::module_surface::VERSION_KEY)?;
            crate::cpp_dependents::withdraw_marker(store)?;
            crate::java_heads::withdraw_marker(store)?;
            crate::kotlin_surfaces::withdraw_marker(store)?;
            crate::scala_surfaces::withdraw_marker(store)?;
            crate::csharp_surfaces::withdraw_marker(store)?;
            crate::member_surfaces::withdraw_marker(store)?;
            crate::ruby_heads::withdraw_marker(store)?;
            crate::cfml_heads::withdraw_marker(store)?;
            crate::swift_heads::withdraw_marker(store)?;
            crate::objc_heads::withdraw_marker(store)?;
            crate::go_heads::withdraw_marker(store)?;
            crate::rust_receivers::withdraw_marker(store)?;
            crate::php_heads::withdraw_marker(store)?;
            self.surface_dirty = true;
        }
        Ok(())
    }

    /// What an import of an added file can be written as, and the namespaces
    /// and modules the changed files declare (upstream #2392).
    fn import_retry_keys(&self) -> (Vec<String>, Vec<String>) {
        let mut path_keys = BTreeSet::new();
        for file in &self.added_files {
            path_keys.extend(codegraph_store::import_path_keys(file));
        }
        let mut names: Vec<String> = self.module_names.iter().cloned().collect();
        names.sort_unstable();
        (path_keys.into_iter().collect(), names)
    }

    /// Every node of a file left the index: all its names are touched.
    fn note_removed(&mut self, names: HashSet<String>) {
        self.note_all(names);
    }

    /// Touch every one of `names`, as a ref outside their file may now see
    /// them differently.
    fn note_all(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            self.candidates.insert(name.clone());
            self.any.insert(name);
        }
    }

    /// Record each name whose nodes differ between `before` and `after` as a ref
    /// outside the file sees them, comparing the two sides as multisets of
    /// [`candidate_key`]s.
    fn note_candidates(&mut self, before: &[Node], after: &[Node]) {
        let mut keys: BTreeMap<&str, (Vec<String>, Vec<String>)> = BTreeMap::new();
        for node in before {
            keys.entry(&node.name)
                .or_default()
                .0
                .push(candidate_key(node));
        }
        for node in after {
            keys.entry(&node.name)
                .or_default()
                .1
                .push(candidate_key(node));
        }
        let mut touched: HashSet<&str> = HashSet::new();
        for (name, (mut old, mut new)) in keys {
            old.sort_unstable();
            new.sort_unstable();
            if old != new {
                touched.insert(name);
                self.any.insert(name.to_string());
                self.candidates.insert(name.to_string());
            }
        }
        for node in before.iter().chain(after) {
            if crate::rust_receivers::is_receiver_type(node) && touched.contains(node.name.as_str())
            {
                self.rust_receiver_types.insert(node.name.clone());
            }
        }
        // A Rust variant path reaches an enum only while it declares the
        // variant (upstream #2354), which no enum node records: a variant that
        // came, went or changed touches its enum's name.
        for node in before.iter().chain(after) {
            if node.language == Language::Rust
                && node.kind == NodeKind::EnumMember
                && touched.contains(node.name.as_str())
                && let Some((owner, _)) = node.qualified_name.rsplit_once("::")
            {
                let owner = owner.rsplit("::").next().unwrap_or(owner).to_string();
                self.any.insert(owner.clone());
                self.candidates.insert(owner);
            }
        }
    }
}

/// Whether C# `source` may declare an extension method: a parameter list
/// that opens with a `this` parameter, attributes allowed before it
/// (`IsEnum(this Type type)`). Over-approximates (`if (this is X)`), which
/// only re-resolves more references.
fn declares_csharp_extension_method(source: &str) -> bool {
    source.match_indices("this").any(|(at, _)| {
        if !source[at + 4..].starts_with(char::is_whitespace) {
            return false;
        }
        let mut before = source[..at].trim_end();
        while let Some(rest) = before.strip_suffix(']') {
            let Some(open) = rest.rfind('[') else {
                return false;
            };
            before = rest[..open].trim_end();
        }
        before.ends_with('(')
    })
}

/// Each name's [`candidate_key`]s among `nodes`, in the order of the nodes'
/// positions: what a ref in another file sees of them, ordered as
/// `order_candidates` orders one file's candidates.
fn positioned_candidate_keys(nodes: &[Node]) -> BTreeMap<&str, Vec<String>> {
    let mut by_name: BTreeMap<&str, Vec<(i64, i64, String)>> = BTreeMap::new();
    for node in nodes {
        by_name.entry(node.name.as_str()).or_default().push((
            node.start_line,
            node.start_column,
            candidate_key(node),
        ));
    }
    by_name
        .into_iter()
        .map(|(name, mut keyed)| {
            keyed.sort();
            (name, keyed.into_iter().map(|(_, _, key)| key).collect())
        })
        .collect()
}

/// A node as a ref in ANOTHER file sees it during resolution: every field but
/// its id, its position and its timestamp. Two nodes with equal keys are
/// interchangeable candidates for such a ref wherever they sit in their own
/// file, since name matching reads a candidate's line only when the candidate
/// shares the ref's file. A node that cannot be serialized keys by its id, so a
/// move still counts as a change.
fn candidate_key(node: &Node) -> String {
    let mut key = node.clone();
    key.id.clear();
    key.start_line = 0;
    key.end_line = 0;
    key.start_column = 0;
    key.end_column = 0;
    key.updated_at = 0;
    serde_json::to_string(&key).unwrap_or_else(|_| format!("unserializable:{}", node.id))
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

pub(crate) fn modified_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_else(now_millis)
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    thread_local! {
        /// Fail the next sync after its mutations, before resolution.
        pub(crate) static FAIL_BEFORE_RESOLUTION: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
    }

    pub(crate) struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        pub(crate) fn new(name: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            // pid disambiguates parallel `cargo test` binaries; the in-process
            // counter alone resets to 1 per process and would collide in the
            // shared temp dir.
            let path =
                std::env::temp_dir().join(format!("codegraph-{name}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            // State-gated fixture setup: a sync now CLASSIFIES before mutating a
            // row, and a `Missing` namespace legitimately escalates to a forced
            // full migration (frozen plan lines 557-565). Every incremental
            // expectation below therefore starts from an EMPTY, published
            // `Current` namespace, built through the shipped rebuild finalizer
            // rather than by conjuring a raw database file.
            let paths = index_paths(&path).expect("resolve the fixture index namespace");
            let deadline = Instant::now() + Duration::from_secs(30);
            codegraph_store::begin_full_rebuild(
                &paths,
                codegraph_store::RebuildKind::Reindex,
                deadline,
                || false,
            )
            .expect("begin the fixture namespace rebuild")
            .open_store()
            .expect("open the fixture namespace writer")
            .finish()
            .expect("publish the empty fixture namespace as Current");
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

    /// Every edge but `contains` as `file:source -[kind]-> file:target
    /// metadata`, sorted: what a fresh sync of the same files must match.
    fn edge_projection(db: &Path) -> Vec<String> {
        let store = Store::open(db).unwrap();
        let mut stmt = store
            .connection()
            .prepare(
                r#"SELECT src.file_path || ':' || src.qualified_name || ' -[' || e.kind || ']-> '
                    || tgt.file_path || ':' || tgt.qualified_name || ' ' || COALESCE(e.metadata, '')
                FROM edges e
                JOIN nodes src ON src.id = e.source
                JOIN nodes tgt ON tgt.id = e.target
                WHERE e.kind != 'contains'
                ORDER BY 1"#,
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn surface_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(codegraph_resolve::module_surface::VERSION_KEY)
            .unwrap()
    }

    fn write_files(root: &Path, files: &[(&str, &str)]) {
        for (path, contents) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, contents).unwrap();
        }
    }

    /// The edges a sync into an empty index gives `files`.
    fn fresh_edges(name: &str, files: &[(&str, &str)]) -> Vec<String> {
        let dir = TestDir::new(name);
        write_files(dir.path(), files);
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        edge_projection(&db)
    }

    /// A sync with no transaction around it can fail after its mutations
    /// (W4a-F3): the module-surface marker its first mutation withdrew stays
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, reconciles every JavaScript-family file and restores it.
    #[test]
    fn a_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        const A: (&str, &str) = (
            "src/a.ts",
            "export function safeParse(value: unknown) {\n  return value;\n}\n",
        );
        const B: (&str, &str) = (
            "src/b.ts",
            "export function safeParse(value: unknown) {\n  return [value];\n}\n",
        );
        const USE: (&str, &str) = (
            "src/use.ts",
            "import * as z from './external';\n\nexport function check() {\n  return z.core.safeParse(1);\n}\n",
        );
        const TO_A: (&str, &str) = ("src/external.ts", "export * as core from './a';\n");
        const TO_B: (&str, &str) = ("src/external.ts", "export * as core from './b';\n");
        const OTHER: (&str, &str) = ("src/other.ts", "export const other = 1;\n");
        const OTHER_EDITED: (&str, &str) = ("src/other.ts", "export const other = 2;\n");
        let version = Some(codegraph_resolve::module_surface::VERSION.to_string());

        let dir = TestDir::new("surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [A, B, TO_A, USE, OTHER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(surface_marker(&db), version);

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                surface_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        // The barrel moves to `b`; the sync fails before resolving, and a
        // sync with nothing changed reconciles.
        write_files(dir.path(), &[TO_B]);
        fail_once(&["src/external.ts"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(surface_marker(&db), version);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("surface-fail-fresh-b", &[A, B, TO_B, USE, OTHER])
        );

        // Back to `a`, failing again; an unrelated change reconciles.
        write_files(dir.path(), &[TO_A]);
        fail_once(&["src/external.ts"]);
        write_files(dir.path(), &[OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/other.ts"]).unwrap();
        assert_eq!(surface_marker(&db), version);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("surface-fail-fresh-a", &[A, B, TO_A, USE, OTHER_EDITED])
        );
    }

    fn cpp_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::cpp_dependents::VERSION_KEY)
            .unwrap()
    }

    const CPP_LISTS: (&str, &str) = (
        "db/lists.hpp",
        "namespace leveldb {\nclass SkipList {\n public:\n  void Insert(int key) {}\n};\nclass OtherList {\n public:\n  void Insert(int key) {}\n};\n}  // namespace leveldb\n",
    );
    const CPP_LISTS_BODY_EDIT: (&str, &str) = (
        "db/lists.hpp",
        "namespace leveldb {\nclass SkipList {\n public:\n  void Insert(int key) { (void)key; }\n};\nclass OtherList {\n public:\n  void Insert(int key) {}\n};\n}  // namespace leveldb\n",
    );
    const CPP_TO_SKIP: (&str, &str) = (
        "db/aliases.hpp",
        "#include \"db/lists.hpp\"\nnamespace leveldb {\nusing Table = SkipList;\n}  // namespace leveldb\n",
    );
    const CPP_TO_OTHER: (&str, &str) = (
        "db/aliases.hpp",
        "#include \"db/lists.hpp\"\nnamespace leveldb {\nusing Table = OtherList;\n}  // namespace leveldb\n",
    );
    const CPP_USE: (&str, &str) = (
        "db/use.cc",
        "#include \"db/aliases.hpp\"\nnamespace leveldb {\nvoid Fill() {\n  Table table;\n  table.Insert(1);\n}\n}  // namespace leveldb\n",
    );
    const CPP_OTHER: (&str, &str) = ("db/other.cc", "int other() { return 1; }\n");
    const CPP_OTHER_EDITED: (&str, &str) = ("db/other.cc", "int other() { return 2; }\n");

    /// W4-CPP-04: a sync failing after its mutations leaves the C/C++ marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every C/C++ file and restores it: `use.cc`, which
    /// the retargeted alias moves, ends where a fresh index puts it.
    #[test]
    fn a_cpp_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::cpp_dependents::VERSION.to_string());
        let dir = TestDir::new("cpp-surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [CPP_LISTS, CPP_TO_SKIP, CPP_USE, CPP_OTHER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(cpp_marker(&db), version);

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                cpp_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[CPP_TO_OTHER]);
        fail_once(&["db/aliases.hpp"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(cpp_marker(&db), version);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "cpp-surface-fail-fresh-other",
                &[CPP_LISTS, CPP_TO_OTHER, CPP_USE, CPP_OTHER]
            )
        );

        write_files(dir.path(), &[CPP_TO_SKIP]);
        fail_once(&["db/aliases.hpp"]);
        write_files(dir.path(), &[CPP_OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, ["db/other.cc"]).unwrap();
        assert_eq!(cpp_marker(&db), version);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "cpp-surface-fail-fresh-skip",
                &[CPP_LISTS, CPP_TO_SKIP, CPP_USE, CPP_OTHER_EDITED]
            )
        );
    }

    fn java_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::java_heads::VERSION_KEY)
            .unwrap()
    }

    fn head_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::java_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    const JAVA_ROOT: (&str, &str) = (
        "src/app/Root.java",
        "package app;\n\npublic class Root {\n    protected void assist() { }\n}\n",
    );
    const JAVA_OTHER: (&str, &str) = (
        "src/app/Other.java",
        "package app;\n\npublic class Other {\n    protected void assist() { }\n}\n",
    );
    const JAVA_BASE_ROOT: (&str, &str) = (
        "src/app/Base.java",
        "package app;\n\npublic class Base extends Root {\n}\n",
    );
    const JAVA_BASE_OTHER: (&str, &str) = (
        "src/app/Base.java",
        "package app;\n\npublic class Base extends Other {\n}\n",
    );
    const JAVA_CHILD: (&str, &str) = (
        "src/app/Child.java",
        "package app;\n\npublic class Child extends Base {\n    void go() {\n        assist();\n    }\n}\n",
    );
    const JAVA_SPARE: (&str, &str) = (
        "src/app/Spare.java",
        "package app;\n\npublic class Spare { }\n",
    );
    const JAVA_SPARE_EDITED: (&str, &str) = (
        "src/app/Spare.java",
        "package app;\n\npublic class Spare {\n    int count;\n}\n",
    );

    /// W4-JVM-01: a sync failing after its mutations leaves the Java marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Java file and rebuilds the head rows:
    /// `Child`'s bare call, which the changed base moves, ends where a fresh
    /// index puts it.
    #[test]
    fn a_java_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::java_heads::VERSION.to_string());
        let dir = TestDir::new("java-heads-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            JAVA_ROOT,
            JAVA_OTHER,
            JAVA_BASE_ROOT,
            JAVA_CHILD,
            JAVA_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(java_marker(&db), version);

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                java_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[JAVA_BASE_OTHER]);
        fail_once(&["src/app/Base.java"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(java_marker(&db), version);
        let fresh = [
            JAVA_ROOT,
            JAVA_OTHER,
            JAVA_BASE_OTHER,
            JAVA_CHILD,
            JAVA_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("java-heads-fail-other", &fresh)
        );
        assert!(
            edge_projection(&db)
                .iter()
                .any(|edge| edge.contains("Child::go -[calls]-> src/app/Other.java")),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[JAVA_BASE_ROOT]);
        fail_once(&["src/app/Base.java"]);
        write_files(dir.path(), &[JAVA_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.java"]).unwrap();
        assert_eq!(java_marker(&db), version);
        let fresh = [
            JAVA_ROOT,
            JAVA_OTHER,
            JAVA_BASE_ROOT,
            JAVA_CHILD,
            JAVA_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("java-heads-fail-root", &fresh)
        );
    }

    /// W4-JVM-01: an index without the Java marker or its head rows (built
    /// before them) has every Java file re-resolved and every row rebuilt by
    /// its next sync: the bare call an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_java_marker_rebuilds_the_head_rows() {
        let version = Some(crate::java_heads::VERSION.to_string());
        let dir = TestDir::new("java-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            JAVA_ROOT,
            JAVA_OTHER,
            JAVA_BASE_ROOT,
            JAVA_CHILD,
            JAVA_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(java_marker(&db), version);
        let rows = head_rows(&db);
        assert_eq!(
            rows.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            [
                "java_type_heads:src/app/Base.java",
                "java_type_heads:src/app/Child.java"
            ]
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'assist' AND qualified_name = 'app::Root::assist')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::java_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::java_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[JAVA_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.java"]).unwrap();
        assert_eq!(java_marker(&db), version);
        assert_eq!(head_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn kotlin_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::kotlin_surfaces::VERSION_KEY)
            .unwrap()
    }

    fn kotlin_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::kotlin_scope::SURFACE_ROW_KEY_PREFIX)
            .unwrap()
    }

    const KOTLIN_ROOT: (&str, &str) = (
        "src/app/Root.kt",
        "package app\n\nopen class Root {\n    fun assist() { }\n}\n",
    );
    const KOTLIN_OTHER: (&str, &str) = (
        "src/app/Other.kt",
        "package app\n\nopen class Other {\n    fun assist() { }\n}\n",
    );
    const KOTLIN_BASE_ROOT: (&str, &str) = (
        "src/app/Base.kt",
        "package app\n\nopen class Base : Root()\n",
    );
    const KOTLIN_BASE_OTHER: (&str, &str) = (
        "src/app/Base.kt",
        "package app\n\nopen class Base : Other()\n",
    );
    const KOTLIN_CHILD: (&str, &str) = (
        "src/app/Child.kt",
        "package app\n\nclass Child : Base() {\n    fun go() {\n        assist()\n    }\n}\n",
    );
    const KOTLIN_SPARE: (&str, &str) = ("src/app/Spare.kt", "package app\n\nclass Spare\n");
    const KOTLIN_SPARE_EDITED: (&str, &str) = (
        "src/app/Spare.kt",
        "package app\n\nclass Spare {\n    val count = 1\n}\n",
    );

    /// W4-JVM-03: a sync failing after its mutations leaves the Kotlin
    /// marker away, so the next sync, with nothing changed or with an
    /// unrelated change, re-resolves every Kotlin file and rebuilds the
    /// surface rows: `Child`'s bare call, which the changed base moves, ends
    /// where a fresh index puts it.
    #[test]
    fn a_kotlin_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::kotlin_surfaces::VERSION.to_string());
        let dir = TestDir::new("kotlin-surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            KOTLIN_ROOT,
            KOTLIN_OTHER,
            KOTLIN_BASE_ROOT,
            KOTLIN_CHILD,
            KOTLIN_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(kotlin_marker(&db), version);

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                kotlin_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[KOTLIN_BASE_OTHER]);
        fail_once(&["src/app/Base.kt"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(kotlin_marker(&db), version);
        let fresh = [
            KOTLIN_ROOT,
            KOTLIN_OTHER,
            KOTLIN_BASE_OTHER,
            KOTLIN_CHILD,
            KOTLIN_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("kotlin-surface-fail-other", &fresh)
        );
        assert!(
            edge_projection(&db)
                .iter()
                .any(|edge| edge.contains("Child::go -[calls]-> src/app/Other.kt")),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[KOTLIN_BASE_ROOT]);
        fail_once(&["src/app/Base.kt"]);
        write_files(dir.path(), &[KOTLIN_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.kt"]).unwrap();
        assert_eq!(kotlin_marker(&db), version);
        let fresh = [
            KOTLIN_ROOT,
            KOTLIN_OTHER,
            KOTLIN_BASE_ROOT,
            KOTLIN_CHILD,
            KOTLIN_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("kotlin-surface-fail-root", &fresh)
        );
    }

    /// W4-JVM-03: an index without the Kotlin marker or its surface rows
    /// (built before them) has every Kotlin file re-resolved and every row
    /// rebuilt by its next sync: the bare call an older sync left without
    /// its edge comes back.
    #[test]
    fn an_index_without_the_kotlin_marker_rebuilds_the_surface_rows() {
        let version = Some(crate::kotlin_surfaces::VERSION.to_string());
        let dir = TestDir::new("kotlin-surface-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            KOTLIN_ROOT,
            KOTLIN_OTHER,
            KOTLIN_BASE_ROOT,
            KOTLIN_CHILD,
            KOTLIN_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(kotlin_marker(&db), version);
        let rows = kotlin_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "kotlin_surface:src/app/Base.kt"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'app::Root::assist')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::kotlin_scope::SURFACE_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::kotlin_surfaces::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[KOTLIN_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.kt"]).unwrap();
        assert_eq!(kotlin_marker(&db), version);
        let mut rebuilt = kotlin_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "kotlin_surface:src/app/Spare.kt");
        wanted.retain(|(key, _)| key != "kotlin_surface:src/app/Spare.kt");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    fn scala_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::scala_surfaces::VERSION_KEY)
            .unwrap()
    }

    fn scala_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::scala_scope::SURFACE_ROW_KEY_PREFIX)
            .unwrap()
    }

    const SCALA_ROOT: (&str, &str) = (
        "src/app/Root.scala",
        "package app\n\ntrait Root {\n  def assist(): Int = 1\n}\n",
    );
    const SCALA_OTHER: (&str, &str) = (
        "src/app/Other.scala",
        "package app\n\ntrait Other {\n  def assist(): Int = 2\n}\n",
    );
    const SCALA_BASE_ROOT: (&str, &str) = (
        "src/app/Base.scala",
        "package app\n\ntrait Base extends Root\n",
    );
    const SCALA_BASE_OTHER: (&str, &str) = (
        "src/app/Base.scala",
        "package app\n\ntrait Base extends Other\n",
    );
    const SCALA_CHILD: (&str, &str) = (
        "src/app/Child.scala",
        "package app\n\nclass Child extends Base {\n  def go(): Int = assist()\n}\n",
    );
    const SCALA_SPARE: (&str, &str) = ("src/app/Spare.scala", "package app\n\nclass Spare\n");
    const SCALA_SPARE_EDITED: (&str, &str) = (
        "src/app/Spare.scala",
        "package app\n\nclass Spare {\n  val count = 1\n}\n",
    );

    /// W4-SC-01: a sync failing after its mutations leaves the Scala marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Scala file and rebuilds the surface rows:
    /// `Child`'s bare call, which the changed base moves, ends where a fresh
    /// index puts it.
    #[test]
    fn a_scala_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::scala_surfaces::VERSION.to_string());
        let dir = TestDir::new("scala-surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            SCALA_ROOT,
            SCALA_OTHER,
            SCALA_BASE_ROOT,
            SCALA_CHILD,
            SCALA_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(scala_marker(&db), version);

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                scala_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[SCALA_BASE_OTHER]);
        fail_once(&["src/app/Base.scala"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(scala_marker(&db), version);
        let fresh = [
            SCALA_ROOT,
            SCALA_OTHER,
            SCALA_BASE_OTHER,
            SCALA_CHILD,
            SCALA_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("scala-surface-fail-other", &fresh)
        );
        assert!(
            edge_projection(&db)
                .iter()
                .any(|edge| edge.contains("Child::go -[calls]-> src/app/Other.scala")),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[SCALA_BASE_ROOT]);
        fail_once(&["src/app/Base.scala"]);
        write_files(dir.path(), &[SCALA_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.scala"]).unwrap();
        assert_eq!(scala_marker(&db), version);
        let fresh = [
            SCALA_ROOT,
            SCALA_OTHER,
            SCALA_BASE_ROOT,
            SCALA_CHILD,
            SCALA_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("scala-surface-fail-root", &fresh)
        );
    }

    /// W4-SC-01: an index without the Scala marker or its surface rows (built
    /// before them) has every Scala file re-resolved and every row rebuilt by
    /// its next sync: the bare call an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_scala_marker_rebuilds_the_surface_rows() {
        let version = Some(crate::scala_surfaces::VERSION.to_string());
        let dir = TestDir::new("scala-surface-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            SCALA_ROOT,
            SCALA_OTHER,
            SCALA_BASE_ROOT,
            SCALA_CHILD,
            SCALA_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(scala_marker(&db), version);
        let rows = scala_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "scala_surface:src/app/Base.scala"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Root::assist')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::scala_scope::SURFACE_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::scala_surfaces::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[SCALA_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/app/Spare.scala"]).unwrap();
        assert_eq!(scala_marker(&db), version);
        let mut rebuilt = scala_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "scala_surface:src/app/Spare.scala");
        wanted.retain(|(key, _)| key != "scala_surface:src/app/Spare.scala");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    fn csharp_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::csharp_surfaces::VERSION_KEY)
            .unwrap()
    }

    fn csharp_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::csharp_types::SURFACE_ROW_KEY_PREFIX)
            .unwrap()
    }

    const CSHARP_BASE: (&str, &str) = (
        "src/Lib/Base.cs",
        "namespace Lib\n{\n    public class Reader\n    {\n        internal enum State { Start, Done }\n    }\n}\n",
    );
    const CSHARP_MIDDLE_READER: (&str, &str) = (
        "src/Lib/Middle.cs",
        "namespace Lib\n{\n    public class Middle : Reader\n    {\n    }\n}\n",
    );
    const CSHARP_MIDDLE_ALONE: (&str, &str) = (
        "src/Lib/Middle.cs",
        "namespace Lib\n{\n    public class Middle\n    {\n    }\n}\n",
    );
    const CSHARP_LEAF: (&str, &str) = (
        "src/Lib/Leaf.cs",
        "namespace Lib\n{\n    public class Leaf : Middle\n    {\n        object Peek() => State.Start;\n    }\n}\n",
    );
    const CSHARP_SPARE: (&str, &str) = (
        "src/Lib/Spare.cs",
        "namespace Lib\n{\n    public class Spare\n    {\n    }\n}\n",
    );
    const CSHARP_SPARE_EDITED: (&str, &str) = (
        "src/Lib/Spare.cs",
        "namespace Lib\n{\n    public class Spare\n    {\n        int Count() => 1;\n    }\n}\n",
    );

    fn leaf_reaches_state(db: &Path) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains("Leaf::Peek -[references]-> src/Lib/Base.cs"))
    }

    /// W4-CS-01: a sync failing after its mutations leaves the C# marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every C# file and rebuilds the surface rows:
    /// `Leaf`'s reference to the nested `State`, which the changed middle
    /// type moves, ends where a fresh index puts it.
    #[test]
    fn a_csharp_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::csharp_surfaces::VERSION.to_string());
        let dir = TestDir::new("csharp-surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [CSHARP_BASE, CSHARP_MIDDLE_READER, CSHARP_LEAF, CSHARP_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(csharp_marker(&db), version);
        assert!(leaf_reaches_state(&db), "{:#?}", edge_projection(&db));

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                csharp_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[CSHARP_MIDDLE_ALONE]);
        fail_once(&["src/Lib/Middle.cs"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(csharp_marker(&db), version);
        let fresh = [CSHARP_BASE, CSHARP_MIDDLE_ALONE, CSHARP_LEAF, CSHARP_SPARE];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("csharp-surface-fail-alone", &fresh)
        );
        assert!(!leaf_reaches_state(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[CSHARP_MIDDLE_READER]);
        fail_once(&["src/Lib/Middle.cs"]);
        write_files(dir.path(), &[CSHARP_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/Lib/Spare.cs"]).unwrap();
        assert_eq!(csharp_marker(&db), version);
        let fresh = [
            CSHARP_BASE,
            CSHARP_MIDDLE_READER,
            CSHARP_LEAF,
            CSHARP_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("csharp-surface-fail-reader", &fresh)
        );
        assert!(leaf_reaches_state(&db), "{:#?}", edge_projection(&db));
    }

    /// W4-CS-01: an index without the C# marker or its surface rows (built
    /// before them) has every C# file re-resolved and every row rebuilt by its
    /// next sync: the reference an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_csharp_marker_rebuilds_the_surface_rows() {
        let version = Some(crate::csharp_surfaces::VERSION.to_string());
        let dir = TestDir::new("csharp-surface-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [CSHARP_BASE, CSHARP_MIDDLE_READER, CSHARP_LEAF, CSHARP_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(csharp_marker(&db), version);
        let rows = csharp_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "csharp_surface:src/Lib/Middle.cs"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND target = (SELECT id FROM \
                     nodes WHERE qualified_name = 'Lib::Reader::State')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the reference edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::csharp_types::SURFACE_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::csharp_surfaces::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[CSHARP_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/Lib/Spare.cs"]).unwrap();
        assert_eq!(csharp_marker(&db), version);
        assert_eq!(csharp_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn member_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::member_surfaces::VERSION_KEY)
            .unwrap()
    }

    fn member_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::member_types::SURFACE_ROW_KEY_PREFIX)
            .unwrap()
    }

    const MEMBER_JSON: (&str, &str) = (
        "src/Json/JsonWriter.cs",
        "namespace App.Json;\n\npublic class JsonWriter\n{\n    public void WriteValue(string value) { }\n}\n",
    );
    const MEMBER_OTHER: (&str, &str) = (
        "src/Json/OtherWriter.cs",
        "namespace App.Json;\n\npublic class OtherWriter\n{\n    public void WriteValue(string value) { }\n}\n",
    );
    const MEMBER_BASE_JSON: (&str, &str) = (
        "src/Json/WriterBase.cs",
        "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected JsonWriter Writer { get; set; }\n}\n",
    );
    const MEMBER_BASE_OTHER: (&str, &str) = (
        "src/Json/WriterBase.cs",
        "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected OtherWriter Writer { get; set; }\n}\n",
    );
    const MEMBER_TRACE: (&str, &str) = (
        "src/Json/TraceWriter.cs",
        "namespace App.Json;\n\npublic class TraceWriter : WriterBase\n{\n    public void Trace(string value)\n    {\n        Writer.WriteValue(value);\n    }\n}\n",
    );
    const MEMBER_SPARE: (&str, &str) = (
        "src/Json/Spare.cs",
        "namespace App.Json;\n\npublic class Spare\n{\n}\n",
    );
    const MEMBER_SPARE_EDITED: (&str, &str) = (
        "src/Json/Spare.cs",
        "namespace App.Json;\n\npublic class Spare\n{\n    int Count() => 1;\n}\n",
    );

    fn trace_reaches(db: &Path, file: &str) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains(&format!("TraceWriter::Trace -[calls]-> {file}")))
    }

    /// W4-CS-03: a sync failing after its mutations leaves the member marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every member-typed file and rebuilds the rows:
    /// `TraceWriter`'s call through the member its base declares ends where
    /// a fresh index puts it.
    #[test]
    fn a_member_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::member_surfaces::VERSION.to_string());
        let dir = TestDir::new("member-surface-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            MEMBER_JSON,
            MEMBER_OTHER,
            MEMBER_BASE_JSON,
            MEMBER_TRACE,
            MEMBER_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(member_marker(&db), version);
        assert!(
            trace_reaches(&db, "src/Json/JsonWriter.cs"),
            "{:#?}",
            edge_projection(&db)
        );

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                member_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[MEMBER_BASE_OTHER]);
        fail_once(&["src/Json/WriterBase.cs"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(member_marker(&db), version);
        let fresh = [
            MEMBER_JSON,
            MEMBER_OTHER,
            MEMBER_BASE_OTHER,
            MEMBER_TRACE,
            MEMBER_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("member-surface-fail-other", &fresh)
        );
        assert!(
            trace_reaches(&db, "src/Json/OtherWriter.cs"),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[MEMBER_BASE_JSON]);
        fail_once(&["src/Json/WriterBase.cs"]);
        write_files(dir.path(), &[MEMBER_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/Json/Spare.cs"]).unwrap();
        assert_eq!(member_marker(&db), version);
        let fresh = [
            MEMBER_JSON,
            MEMBER_OTHER,
            MEMBER_BASE_JSON,
            MEMBER_TRACE,
            MEMBER_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("member-surface-fail-json", &fresh)
        );
        assert!(
            trace_reaches(&db, "src/Json/JsonWriter.cs"),
            "{:#?}",
            edge_projection(&db)
        );
    }

    /// W4-CS-03: an index without the member marker or its rows (built before
    /// them) has every member-typed file re-resolved and every row rebuilt by
    /// its next sync: the call an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_member_marker_rebuilds_the_surface_rows() {
        let version = Some(crate::member_surfaces::VERSION.to_string());
        let dir = TestDir::new("member-surface-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            MEMBER_JSON,
            MEMBER_OTHER,
            MEMBER_BASE_JSON,
            MEMBER_TRACE,
            MEMBER_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(member_marker(&db), version);
        let rows = member_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "member_surface:src/Json/WriterBase.cs"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'App.Json::JsonWriter::WriteValue')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::member_types::SURFACE_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::member_surfaces::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[MEMBER_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["src/Json/Spare.cs"]).unwrap();
        assert_eq!(member_marker(&db), version);
        let mut rebuilt = member_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "member_surface:src/Json/Spare.cs");
        wanted.retain(|(key, _)| key != "member_surface:src/Json/Spare.cs");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    fn ruby_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::ruby_heads::VERSION_KEY)
            .unwrap()
    }

    fn ruby_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::ruby_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    const RUBY_HELPERS: (&str, &str) = (
        "lib/helpers.rb",
        "module Helpers\n  def helper\n    1\n  end\nend\n",
    );
    const RUBY_OTHER_HELPERS: (&str, &str) = (
        "lib/other_helpers.rb",
        "module OtherHelpers\n  def helper\n    2\n  end\nend\n",
    );
    const RUBY_BASE_HELPERS: (&str, &str) = ("lib/base.rb", "class Base\n  include Helpers\nend\n");
    const RUBY_BASE_OTHER: (&str, &str) =
        ("lib/base.rb", "class Base\n  include OtherHelpers\nend\n");
    const RUBY_FOO: (&str, &str) = (
        "lib/foo.rb",
        "class Foo < Base\n  def run\n    helper()\n  end\nend\n",
    );
    const RUBY_SPARE: (&str, &str) = ("lib/spare.rb", "class Spare\nend\n");
    const RUBY_SPARE_EDITED: (&str, &str) = (
        "lib/spare.rb",
        "class Spare\n  def count\n    1\n  end\nend\n",
    );

    fn ruby_run_reaches(db: &Path, file: &str) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains(&format!("Foo::run -[calls]-> {file}")))
    }

    /// W4-RB-01: a sync failing after its mutations leaves the Ruby marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Ruby file and rebuilds the rows: `Foo`'s
    /// bare call through the module its superclass mixes in ends where a
    /// fresh index puts it.
    #[test]
    fn a_ruby_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::ruby_heads::VERSION.to_string());
        let dir = TestDir::new("ruby-heads-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            RUBY_HELPERS,
            RUBY_OTHER_HELPERS,
            RUBY_BASE_HELPERS,
            RUBY_FOO,
            RUBY_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(ruby_marker(&db), version);
        assert!(
            ruby_run_reaches(&db, "lib/helpers.rb"),
            "{:#?}",
            edge_projection(&db)
        );

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                ruby_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[RUBY_BASE_OTHER]);
        fail_once(&["lib/base.rb"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(ruby_marker(&db), version);
        let fresh = [
            RUBY_HELPERS,
            RUBY_OTHER_HELPERS,
            RUBY_BASE_OTHER,
            RUBY_FOO,
            RUBY_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("ruby-heads-fail-other", &fresh)
        );
        assert!(
            ruby_run_reaches(&db, "lib/other_helpers.rb"),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[RUBY_BASE_HELPERS]);
        fail_once(&["lib/base.rb"]);
        write_files(dir.path(), &[RUBY_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["lib/spare.rb"]).unwrap();
        assert_eq!(ruby_marker(&db), version);
        let fresh = [
            RUBY_HELPERS,
            RUBY_OTHER_HELPERS,
            RUBY_BASE_HELPERS,
            RUBY_FOO,
            RUBY_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("ruby-heads-fail-helpers", &fresh)
        );
        assert!(
            ruby_run_reaches(&db, "lib/helpers.rb"),
            "{:#?}",
            edge_projection(&db)
        );
    }

    /// W4-RB-01: an index without the Ruby marker or its rows (built before
    /// them) has every Ruby file re-resolved and every row rebuilt by its
    /// next sync: the bare call an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_ruby_marker_rebuilds_the_head_rows() {
        let version = Some(crate::ruby_heads::VERSION.to_string());
        let dir = TestDir::new("ruby-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            RUBY_HELPERS,
            RUBY_OTHER_HELPERS,
            RUBY_BASE_HELPERS,
            RUBY_FOO,
            RUBY_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(ruby_marker(&db), version);
        let rows = ruby_rows(&db);
        assert!(
            rows.iter().any(|(key, _)| key == "ruby_heads:lib/base.rb"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Helpers::helper')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::ruby_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::ruby_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[RUBY_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["lib/spare.rb"]).unwrap();
        assert_eq!(ruby_marker(&db), version);
        let mut rebuilt = ruby_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "ruby_heads:lib/spare.rb");
        wanted.retain(|(key, _)| key != "ruby_heads:lib/spare.rb");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    fn cfml_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::cfml_heads::VERSION_KEY)
            .unwrap()
    }

    fn cfml_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::cfml_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    const CFML_FIRST: (&str, &str) = (
        "system/First.cfc",
        "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
    );
    const CFML_SECOND: (&str, &str) = (
        "system/Second.cfc",
        "component {\n  function getSetting( name ) {\n    return 2;\n  }\n}\n",
    );
    const CFML_MIDDLE_FIRST: (&str, &str) =
        ("system/Middle.cfc", "component extends=\"First\" {\n}\n");
    const CFML_MIDDLE_SECOND: (&str, &str) =
        ("system/Middle.cfc", "component extends=\"Second\" {\n}\n");
    const CFML_HANDLER: (&str, &str) = (
        "system/Handler.cfc",
        "component extends=\"Middle\" {\n  function index() {\n    return getSetting( \"appName\" );\n  }\n}\n",
    );
    const CFML_SPARE: (&str, &str) = ("system/Spare.cfc", "component {\n}\n");
    const CFML_SPARE_EDITED: (&str, &str) = (
        "system/Spare.cfc",
        "component {\n  function count() {\n    return 1;\n  }\n}\n",
    );

    fn cfml_index_reaches(db: &Path, file: &str) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains(&format!("Handler::index -[calls]-> {file}")))
    }

    /// W4-CF-01: a sync failing after its mutations leaves the CFML marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every `.cfc` file and rebuilds the rows: the
    /// handler's bare call through the chain its middle component extends
    /// ends where a fresh index puts it.
    #[test]
    fn a_cfml_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::cfml_heads::VERSION.to_string());
        let dir = TestDir::new("cfml-heads-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            CFML_FIRST,
            CFML_SECOND,
            CFML_MIDDLE_FIRST,
            CFML_HANDLER,
            CFML_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(cfml_marker(&db), version);
        assert!(
            cfml_index_reaches(&db, "system/First.cfc"),
            "{:#?}",
            edge_projection(&db)
        );

        let fail_once = |paths: &[&str]| {
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                cfml_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );
        };

        write_files(dir.path(), &[CFML_MIDDLE_SECOND]);
        fail_once(&["system/Middle.cfc"]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(cfml_marker(&db), version);
        let fresh = [
            CFML_FIRST,
            CFML_SECOND,
            CFML_MIDDLE_SECOND,
            CFML_HANDLER,
            CFML_SPARE,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("cfml-heads-fail-second", &fresh)
        );
        assert!(
            cfml_index_reaches(&db, "system/Second.cfc"),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[CFML_MIDDLE_FIRST]);
        fail_once(&["system/Middle.cfc"]);
        write_files(dir.path(), &[CFML_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["system/Spare.cfc"]).unwrap();
        assert_eq!(cfml_marker(&db), version);
        let fresh = [
            CFML_FIRST,
            CFML_SECOND,
            CFML_MIDDLE_FIRST,
            CFML_HANDLER,
            CFML_SPARE_EDITED,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("cfml-heads-fail-first", &fresh)
        );
        assert!(
            cfml_index_reaches(&db, "system/First.cfc"),
            "{:#?}",
            edge_projection(&db)
        );
    }

    const CFML_USER_A: (&str, &str) = (
        "a/UserService.cfc",
        "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
    );
    const CFML_USER_B: (&str, &str) = (
        "b/UserService.cfc",
        "component {\n  function save(any u) {\n    return u;\n  }\n}\n",
    );
    const CFML_TYPED_TEMPLATE: (&str, &str) = (
        "views/page.cfm",
        "<cfset svc = new UserService()>\n<cfoutput>#svc.save(1)#</cfoutput>\n",
    );
    const CFML_TYPED_SCRIPT: (&str, &str) =
        ("views/run.cfs", "svc = new UserService();\nsvc.save(2);\n");

    /// W4-CF-02: a sync that removes one of two same-named components and
    /// fails before resolving leaves the typed calls of a template and a
    /// script parked; the next sync, with nothing changed or an unrelated
    /// change, re-resolves every CFML file, so they reach the remaining
    /// component as a fresh index has them.
    #[test]
    fn a_cfml_typed_call_parked_by_a_failed_sync_reaches_the_remaining_namesake() {
        let reaches = |db: &Path| {
            edge_projection(db)
                .iter()
                .filter(|edge| edge.contains("-[calls]-> a/UserService.cfc"))
                .count()
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "cfml-typed-fail-unrelated"
            } else {
                "cfml-typed-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [
                CFML_USER_A,
                CFML_USER_B,
                CFML_TYPED_TEMPLATE,
                CFML_TYPED_SCRIPT,
                CFML_SPARE,
            ];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(reaches(&db), 0, "{:#?}", edge_projection(&db));

            std::fs::remove_file(dir.path().join(CFML_USER_B.0)).unwrap();
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [CFML_USER_B.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                cfml_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[CFML_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [CFML_SPARE.0]).unwrap();
                CFML_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                CFML_SPARE
            };
            assert_eq!(
                cfml_marker(&db),
                Some(crate::cfml_heads::VERSION.to_string())
            );
            let fresh = [CFML_USER_A, CFML_TYPED_TEMPLATE, CFML_TYPED_SCRIPT, spare];
            let name = if unrelated {
                "cfml-typed-fresh-unrelated"
            } else {
                "cfml-typed-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert_eq!(reaches(&db), 2, "{:#?}", edge_projection(&db));
        }
    }

    fn swift_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::swift_heads::VERSION_KEY)
            .unwrap()
    }

    const SWIFT_CHANNEL: (&str, &str) = (
        "Sources/Core/Channel.swift",
        "class BaseChannel {\n    enum ReadResult { case some, none }\n}\nclass OtherChannel {\n}\n",
    );
    const SWIFT_MIDDLE_BASE: (&str, &str) = (
        "Sources/Core/Middle.swift",
        "class MiddleChannel: BaseChannel {\n}\n",
    );
    const SWIFT_MIDDLE_OTHER: (&str, &str) = (
        "Sources/Core/Middle.swift",
        "class MiddleChannel: OtherChannel {\n}\n",
    );
    const SWIFT_STREAM: (&str, &str) = (
        "Sources/Core/Stream.swift",
        "final class StreamChannel: MiddleChannel {\n    var last: ReadResult?\n}\n",
    );
    const SWIFT_SPARE: (&str, &str) = ("Sources/Core/Spare.swift", "struct Spare {\n}\n");
    const SWIFT_SPARE_EDITED: (&str, &str) = (
        "Sources/Core/Spare.swift",
        "struct Spare {\n    let count = 1\n}\n",
    );

    /// W4-SW-01: a sync failing after its mutations leaves the Swift marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Swift file and rebuilds the rows: the
    /// subclass's bare nested type, out of scope once the middle class names
    /// another superclass, ends where a fresh index puts it.
    #[test]
    fn a_swift_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let in_scope = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("StreamChannel -[references]-> Sources/Core/Channel.swift:BaseChannel::ReadResult"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "swift-heads-fail-unrelated"
            } else {
                "swift-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [SWIFT_CHANNEL, SWIFT_MIDDLE_BASE, SWIFT_STREAM, SWIFT_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                swift_marker(&db),
                Some(crate::swift_heads::VERSION.to_string())
            );
            assert!(in_scope(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[SWIFT_MIDDLE_OTHER]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [SWIFT_MIDDLE_OTHER.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                swift_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[SWIFT_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [SWIFT_SPARE.0]).unwrap();
                SWIFT_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                SWIFT_SPARE
            };
            assert_eq!(
                swift_marker(&db),
                Some(crate::swift_heads::VERSION.to_string())
            );
            let fresh = [SWIFT_CHANNEL, SWIFT_MIDDLE_OTHER, SWIFT_STREAM, spare];
            let name = if unrelated {
                "swift-heads-fresh-unrelated"
            } else {
                "swift-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(!in_scope(&db), "{:#?}", edge_projection(&db));
        }
    }

    fn objc_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::objc_heads::VERSION_KEY)
            .unwrap()
    }

    const OBJC_BASE: (&str, &str) = (
        "Core/SDBase.m",
        "@interface SDBase : NSObject\n@end\n@implementation SDBase\n- (void)prepare {\n}\n@end\n",
    );
    const OBJC_OTHER: (&str, &str) = (
        "Core/SDOther.m",
        "@interface SDOther : NSObject\n@end\n@implementation SDOther\n- (void)prepare {\n}\n@end\n",
    );
    const OBJC_MIDDLE_BASE: (&str, &str) =
        ("Core/SDMiddle.h", "@interface SDMiddle : SDBase\n@end\n");
    const OBJC_MIDDLE_OTHER: (&str, &str) =
        ("Core/SDMiddle.h", "@interface SDMiddle : SDOther\n@end\n");
    const OBJC_CACHE: (&str, &str) = (
        "Core/SDCache.m",
        "@interface SDCache : SDMiddle\n@end\n@implementation SDCache\n- (void)run {\n    [self prepare];\n}\n@end\n",
    );
    const OBJC_SPARE: (&str, &str) = ("Core/SDSpare.m", "@interface SDSpare : NSObject\n@end\n");
    const OBJC_SPARE_EDITED: (&str, &str) = (
        "Core/SDSpare.m",
        "@interface SDSpare : NSObject\n@property (nonatomic) NSURL *url;\n@end\n",
    );

    /// W4-SW-04: a sync failing after its mutations leaves the Objective-C
    /// marker away, so the next sync, with nothing changed or with an
    /// unrelated change, re-resolves every Objective-C file and rebuilds the
    /// rows: the subclass's `[self prepare]`, whose hierarchy now passes the
    /// other base, ends where a fresh index puts it.
    #[test]
    fn an_objc_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_other = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.contains("SDCache::run -[calls]-> Core/SDOther.m:SDOther::prepare")
            })
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "objc-heads-fail-unrelated"
            } else {
                "objc-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [
                OBJC_BASE,
                OBJC_OTHER,
                OBJC_MIDDLE_BASE,
                OBJC_CACHE,
                OBJC_SPARE,
            ];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                objc_marker(&db),
                Some(crate::objc_heads::VERSION.to_string())
            );
            assert!(!to_other(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[OBJC_MIDDLE_OTHER]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [OBJC_MIDDLE_OTHER.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                objc_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[OBJC_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [OBJC_SPARE.0]).unwrap();
                OBJC_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                OBJC_SPARE
            };
            assert_eq!(
                objc_marker(&db),
                Some(crate::objc_heads::VERSION.to_string())
            );
            let fresh = [OBJC_BASE, OBJC_OTHER, OBJC_MIDDLE_OTHER, OBJC_CACHE, spare];
            let name = if unrelated {
                "objc-heads-fresh-unrelated"
            } else {
                "objc-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_other(&db), "{:#?}", edge_projection(&db));
        }
    }

    fn objc_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::objc_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    /// W4-SW-04: an index whose Objective-C marker and rows alone are gone
    /// (the other markers written back) has every Objective-C file
    /// re-resolved and every row rebuilt by its next sync, even one with
    /// nothing changed: the self-send an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_objc_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::objc_heads::VERSION.to_string());
        let dir = TestDir::new("objc-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            OBJC_BASE,
            OBJC_OTHER,
            OBJC_MIDDLE_BASE,
            OBJC_CACHE,
            OBJC_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(objc_marker(&db), version);
        let rows = objc_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "objc_heads:Core/SDMiddle.h"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'SDBase::prepare')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::objc_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::objc_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            swift_marker(&db),
            Some(crate::swift_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(objc_marker(&db), version);
        assert_eq!(objc_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn php_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::php_heads::VERSION_KEY)
            .unwrap()
    }

    const PHP_BASE: (&str, &str) = (
        "src/Base.php",
        "<?php\nnamespace App;\n\nclass Base\n{\n    public function prepare() {}\n}\n",
    );
    const PHP_OTHER: (&str, &str) = (
        "src/Other.php",
        "<?php\nnamespace App;\n\nclass Other\n{\n    public function prepare() {}\n}\n",
    );
    const PHP_MIDDLE_BASE: (&str, &str) = (
        "src/Middle.php",
        "<?php\nnamespace App;\n\nclass Middle extends Base\n{\n}\n",
    );
    const PHP_MIDDLE_OTHER: (&str, &str) = (
        "src/Middle.php",
        "<?php\nnamespace App;\n\nclass Middle extends Other\n{\n}\n",
    );
    const PHP_CHILD: (&str, &str) = (
        "src/Child.php",
        "<?php\nnamespace App;\n\nclass Child extends Middle\n{\n    public function run()\n    {\n        $this->prepare();\n    }\n}\n",
    );
    const PHP_SPARE: (&str, &str) = (
        "src/Spare.php",
        "<?php\nnamespace App;\n\nclass Spare\n{\n}\n",
    );
    const PHP_SPARE_EDITED: (&str, &str) = (
        "src/Spare.php",
        "<?php\nnamespace App;\n\nclass Spare\n{\n    public function idle() {}\n}\n",
    );

    /// W4-PHP-01: a sync failing after its mutations leaves the PHP marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every PHP file and rebuilds the rows: the
    /// subclass's `$this->prepare()`, whose ancestry now passes the other
    /// base, ends where a fresh index puts it.
    #[test]
    fn a_php_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_other = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.contains("App::Child::run -[calls]-> src/Other.php:App::Other::prepare")
            })
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "php-heads-fail-unrelated"
            } else {
                "php-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [PHP_BASE, PHP_OTHER, PHP_MIDDLE_BASE, PHP_CHILD, PHP_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(php_marker(&db), Some(crate::php_heads::VERSION.to_string()));
            assert!(!to_other(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[PHP_MIDDLE_OTHER]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [PHP_MIDDLE_OTHER.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                php_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[PHP_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [PHP_SPARE.0]).unwrap();
                PHP_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                PHP_SPARE
            };
            assert_eq!(php_marker(&db), Some(crate::php_heads::VERSION.to_string()));
            let fresh = [PHP_BASE, PHP_OTHER, PHP_MIDDLE_OTHER, PHP_CHILD, spare];
            let name = if unrelated {
                "php-heads-fresh-unrelated"
            } else {
                "php-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_other(&db), "{:#?}", edge_projection(&db));
        }
    }

    fn php_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::php_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    /// W4-PHP-01: an index whose PHP marker and rows alone are gone (the
    /// other markers written back) has every PHP file re-resolved and every
    /// row rebuilt by its next sync, even one with nothing changed: the
    /// `$this` call an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_php_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::php_heads::VERSION.to_string());
        let dir = TestDir::new("php-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [PHP_BASE, PHP_OTHER, PHP_MIDDLE_BASE, PHP_CHILD, PHP_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(php_marker(&db), version);
        let rows = php_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "php_heads:src/Middle.php"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'App::Base::prepare')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::php_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::php_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            objc_marker(&db),
            Some(crate::objc_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(php_marker(&db), version);
        assert_eq!(php_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn go_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::go_heads::VERSION_KEY)
            .unwrap()
    }

    fn go_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::go_packages::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    const GO_MOD: (&str, &str) = ("go.mod", "module example.com/app\n\ngo 1.22\n");
    const GO_BASE: (&str, &str) = (
        "h/base.go",
        "package h\n\ntype BaseAPI struct{}\n\nfunc (b *BaseAPI) SendError() {}\n\ntype OtherAPI struct{}\n\nfunc (o *OtherAPI) SendError() {}\n",
    );
    const GO_SERVER_BASE: (&str, &str) = (
        "h/server.go",
        "package h\n\ntype server struct {\n\tBaseAPI\n}\n",
    );
    const GO_SERVER_OTHER: (&str, &str) = (
        "h/server.go",
        "package h\n\ntype server struct {\n\tOtherAPI\n}\n",
    );
    const GO_RUN: (&str, &str) = (
        "h/run.go",
        "package h\n\nfunc (s *server) Run() {\n\ts.SendError()\n}\n",
    );
    const GO_SPARE: (&str, &str) = ("h/spare.go", "package h\n\ntype spare struct{}\n");
    const GO_SPARE_EDITED: (&str, &str) = (
        "h/spare.go",
        "package h\n\ntype spare struct {\n\tcount int\n}\n",
    );

    /// W4-GO-01: a sync failing after its mutations leaves the Go marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Go file and rebuilds the rows: the call a
    /// changed embedding moves ends where a fresh index puts it.
    #[test]
    fn a_go_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_other = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("server::Run -[calls]-> h/base.go:OtherAPI::SendError"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "go-heads-fail-unrelated"
            } else {
                "go-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [GO_MOD, GO_BASE, GO_SERVER_BASE, GO_RUN, GO_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(go_marker(&db), Some(crate::go_heads::VERSION.to_string()));
            assert!(!to_other(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[GO_SERVER_OTHER]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [GO_SERVER_OTHER.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(go_marker(&db), None, "a failed sync leaves the marker away");

            let spare = if unrelated {
                write_files(dir.path(), &[GO_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [GO_SPARE.0]).unwrap();
                GO_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                GO_SPARE
            };
            assert_eq!(go_marker(&db), Some(crate::go_heads::VERSION.to_string()));
            let fresh = [GO_MOD, GO_BASE, GO_SERVER_OTHER, GO_RUN, spare];
            let name = if unrelated {
                "go-heads-fresh-unrelated"
            } else {
                "go-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_other(&db), "{:#?}", edge_projection(&db));
        }
    }

    const RS_MANIFEST: (&str, &str) = (
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    const RS_LIB: (&str, &str) = (
        "src/lib.rs",
        "mod widget;\nmod alias;\nmod run;\nmod spare;\n",
    );
    const RS_WIDGET: (&str, &str) = (
        "src/widget.rs",
        "pub struct Widget;\n\nimpl Widget {\n    pub fn fresh() -> Self {\n        Widget\n    }\n}\n",
    );
    const RS_NO_ALIAS: (&str, &str) = ("src/alias.rs", "pub fn unrelated() {}\n");
    const RS_ALIAS: (&str, &str) = (
        "src/alias.rs",
        "pub type WidgetRef = crate::widget::Widget;\n\npub fn unrelated() {}\n",
    );
    const RS_RUN: (&str, &str) = (
        "src/run.rs",
        "use crate::alias::WidgetRef;\n\npub fn run() {\n    let _w = WidgetRef::fresh();\n}\n",
    );
    const RS_SPARE: (&str, &str) = ("src/spare.rs", "pub fn spare() {}\n");
    const RS_SPARE_EDITED: (&str, &str) = ("src/spare.rs", "pub fn spare() -> u8 {\n    1\n}\n");

    fn rust_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::rust_receivers::VERSION_KEY)
            .unwrap()
    }

    /// W4-RS-03: a sync that fails after the receiver lane re-parked the
    /// references written through a type that came leaves the marker away,
    /// and the next sync — with nothing or something unrelated changed —
    /// re-resolves every Rust file to what a fresh index holds.
    #[test]
    fn a_rust_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_fresh = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("run -[calls]-> src/widget.rs:Widget::fresh"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "rust-receivers-fail-unrelated"
            } else {
                "rust-receivers-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [
                RS_MANIFEST,
                RS_LIB,
                RS_WIDGET,
                RS_NO_ALIAS,
                RS_RUN,
                RS_SPARE,
            ];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                rust_marker(&db),
                Some(crate::rust_receivers::VERSION.to_string())
            );
            assert!(!to_fresh(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[RS_ALIAS]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [RS_ALIAS.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                rust_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[RS_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [RS_SPARE.0]).unwrap();
                RS_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                RS_SPARE
            };
            assert_eq!(
                rust_marker(&db),
                Some(crate::rust_receivers::VERSION.to_string())
            );
            let fresh = [RS_MANIFEST, RS_LIB, RS_WIDGET, RS_ALIAS, RS_RUN, spare];
            let name = if unrelated {
                "rust-receivers-fresh-unrelated"
            } else {
                "rust-receivers-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_fresh(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W4-RS-03: an index whose Rust marker alone is gone (the other markers
    /// written back) has every Rust file re-resolved by its next sync, even
    /// one with nothing changed.
    #[test]
    fn an_index_without_only_the_rust_marker_reresolves_with_nothing_changed() {
        let version = Some(crate::rust_receivers::VERSION.to_string());
        let dir = TestDir::new("rust-receivers-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RS_MANIFEST, RS_LIB, RS_WIDGET, RS_ALIAS, RS_RUN, RS_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(rust_marker(&db), version);
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Widget::fresh')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata(crate::rust_receivers::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            go_marker(&db),
            Some(crate::go_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(rust_marker(&db), version);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-GO-01: an index whose Go marker and rows alone are gone (the other
    /// markers written back) has every Go file re-resolved and every row
    /// rebuilt by its next sync, even one with nothing changed.
    #[test]
    fn an_index_without_only_the_go_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::go_heads::VERSION.to_string());
        let dir = TestDir::new("go-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [GO_MOD, GO_BASE, GO_SERVER_BASE, GO_RUN, GO_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(go_marker(&db), version);
        let rows = go_rows(&db);
        assert!(
            rows.iter().any(|(key, _)| key == "go_heads:h/server.go"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'BaseAPI::SendError')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::go_packages::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::go_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            objc_marker(&db),
            Some(crate::objc_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(go_marker(&db), version);
        assert_eq!(go_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-CF-01: an index without the CFML marker or its rows (built before
    /// them) has every CFML file re-resolved and every row rebuilt by its
    /// next sync: the bare call an older sync left without its edge comes
    /// back.
    #[test]
    fn an_index_without_the_cfml_marker_rebuilds_the_component_rows() {
        let version = Some(crate::cfml_heads::VERSION.to_string());
        let dir = TestDir::new("cfml-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            CFML_FIRST,
            CFML_SECOND,
            CFML_MIDDLE_FIRST,
            CFML_HANDLER,
            CFML_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(cfml_marker(&db), version);
        let rows = cfml_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "cfml_heads:system/Middle.cfc"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'First::getSetting')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::cfml_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::cfml_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[CFML_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["system/Spare.cfc"]).unwrap();
        assert_eq!(cfml_marker(&db), version);
        let mut rebuilt = cfml_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "cfml_heads:system/Spare.cfc");
        wanted.retain(|(key, _)| key != "cfml_heads:system/Spare.cfc");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-CPP-04: an index without the C/C++ marker (built before it, or
    /// left by a failed sync) has every C/C++ file re-resolved by its next
    /// sync: a stale call edge from an unchanged file is corrected.
    #[test]
    fn an_index_without_the_cpp_marker_re_resolves_every_cpp_file() {
        let version = Some(crate::cpp_dependents::VERSION.to_string());
        let dir = TestDir::new("cpp-surface-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [CPP_LISTS, CPP_TO_SKIP, CPP_USE, CPP_OTHER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let moved = store
                .connection()
                .execute(
                    "UPDATE edges SET target = (SELECT id FROM nodes WHERE qualified_name = \
                     'leveldb::OtherList::Insert') WHERE kind = 'calls' AND target = (SELECT id \
                     FROM nodes WHERE qualified_name = 'leveldb::SkipList::Insert')",
                    [],
                )
                .unwrap();
            assert_eq!(moved, 1, "the call edge to make stale");
            store
                .delete_project_metadata(crate::cpp_dependents::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[CPP_OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, ["db/other.cc"]).unwrap();
        assert_eq!(cpp_marker(&db), version);
        assert_eq!(edge_projection(&db), expected);
    }

    const NS_CONFIG: (&str, &str) = (
        "include/lib/config.h",
        "#pragma once\n#define LIB_BEGIN namespace lib {\n#define LIB_END }\n",
    );
    const NS_RUN: (&str, &str) = (
        "include/lib/run.h",
        "#pragma once\n#include \"lib/config.h\"\nLIB_BEGIN\ninline int run() { return 0; }\nLIB_END\n",
    );
    const NS_MAIN: (&str, &str) = (
        "src/main.cc",
        "#include \"lib/run.h\"\nint main() { return lib::run(); }\n",
    );

    fn namespace_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::cpp_namespaces::ROW_KEY_PREFIX)
            .unwrap()
    }

    /// W4-CPP-07: an index whose C/C++ marker is version 1, built before the
    /// namespace rows existed, has them rebuilt by its next sync, which
    /// re-resolves every C/C++ file and writes version 2: the call through
    /// a macro-opened namespace an older sync left without its edge comes
    /// back.
    #[test]
    fn a_version_one_cpp_marker_rebuilds_the_namespace_rows() {
        let version = Some(crate::cpp_dependents::VERSION.to_string());
        let dir = TestDir::new("cpp-namespace-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [NS_CONFIG, NS_RUN, NS_MAIN, CPP_OTHER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(cpp_marker(&db), version);
        let rows = namespace_rows(&db);
        assert_eq!(
            rows.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            [
                "cpp_namespace_surface:include/lib/config.h",
                "cpp_namespace_surface:include/lib/run.h",
            ]
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'run' AND kind = 'function')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::cpp_namespaces::ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .set_project_metadata(crate::cpp_dependents::VERSION_KEY, "1")
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[CPP_OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, ["db/other.cc"]).unwrap();
        assert_eq!(cpp_marker(&db), version);
        assert_eq!(namespace_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-CPP-04: a header edit that leaves its type surface alone (a
    /// method's body) touches no name, so no file's text is read; an alias
    /// retargeted touches its name.
    #[test]
    fn only_a_changed_type_surface_touches_names() {
        let last = || crate::cpp_dependents::tests::LAST_NAMES.with(|last| last.borrow().clone());
        let dir = TestDir::new("cpp-surface-names");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [CPP_LISTS, CPP_TO_SKIP, CPP_USE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();

        write_files(dir.path(), &[CPP_LISTS_BODY_EDIT]);
        sync_changed_paths(dir.path(), &db, ["db/lists.hpp"]).unwrap();
        assert_eq!(last(), Some(Vec::new()));

        write_files(dir.path(), &[CPP_TO_OTHER]);
        sync_changed_paths(dir.path(), &db, ["db/aliases.hpp"]).unwrap();
        assert_eq!(last(), Some(vec!["Table".to_string()]));
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "cpp-surface-names-fresh",
                &[CPP_LISTS_BODY_EDIT, CPP_TO_OTHER, CPP_USE]
            )
        );
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
        let db = default_db_path(dir.path()).unwrap();

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

    #[test]
    fn changed_paths_lists_sorted_relative_path_deterministically() {
        // Given: two source files freshly written into the project.
        let dir = TestDir::new("watch-changed-paths");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/zeta.ts"),
            "export function zeta() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/alpha.ts"),
            "export function alpha() { return 2; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();

        // When: a sync processes them in reverse-sorted input order.
        let first = sync_changed_paths(dir.path(), &db, ["src/zeta.ts", "src/alpha.ts"]).unwrap();

        // Then: changed_paths is the SORTED relative-path list (not input order).
        assert_eq!(
            first.changed_paths,
            vec!["src/alpha.ts".to_string(), "src/zeta.ts".to_string()],
        );

        // And: a second identical sync (re-touching both) yields the same list,
        // proving determinism across runs despite the HashSet source.
        fs::write(
            dir.path().join("src/zeta.ts"),
            "export function zeta() { return 11; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/alpha.ts"),
            "export function alpha() { return 22; }\n",
        )
        .unwrap();
        let second = sync_changed_paths(dir.path(), &db, ["src/zeta.ts", "src/alpha.ts"]).unwrap();
        assert_eq!(second.changed_paths, first.changed_paths);
    }

    #[test]
    fn changed_paths_empty_when_nothing_reindexed() {
        // Given: a file already indexed once.
        let dir = TestDir::new("watch-changed-paths-empty");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        let _ = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();

        // When: a sync re-checks the same unchanged file.
        let second = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();

        // Then: nothing was reindexed, so changed_paths is empty.
        assert_eq!(second.files_reindexed, 0);
        assert!(second.changed_paths.is_empty());
    }

    #[test]
    fn ignored_and_duplicate_paths_are_counted_and_deduped() {
        // Given: a project with one real source file.
        let dir = TestDir::new("watch-ignored-dup");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        fs::write(dir.path().join("README.md"), "# docs\n").unwrap();
        fs::write(
            dir.path().join("node_modules/pkg/index.ts"),
            "export const x = 1;\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();

        // When: the input mixes an escaping path (ignored via normalize),
        // a node_modules path (ignored via policy), a non-source file
        // (README.md), and the same source file listed twice (deduped).
        let outcome = sync_changed_paths(
            dir.path(),
            &db,
            [
                "../escape.ts",
                "node_modules/pkg/index.ts",
                "README.md",
                "src/app.ts",
                "src/app.ts",
            ],
        )
        .unwrap();

        // Then: the escaping path is ignored before the checked counter; the
        // node_modules + README paths are checked-then-ignored; the duplicate
        // source path is deduped; and exactly one file is reindexed.
        assert_eq!(outcome.files_reindexed, 1);
        assert!(
            outcome.files_ignored >= 3,
            "escape + node_modules + README should be ignored, got {}",
            outcome.files_ignored
        );
        assert_eq!(outcome.changed_paths, vec!["src/app.ts".to_string()]);
    }

    #[test]
    fn deleted_file_is_removed_from_store_on_sync() {
        // Given: a source file indexed once.
        let dir = TestDir::new("watch-delete");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src/app.ts");
        fs::write(&file, "export function answer() { return 42; }\n").unwrap();
        let db = default_db_path(dir.path()).unwrap();
        let first = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(first.files_reindexed, 1);

        // When: the file is deleted on disk and re-synced by path.
        fs::remove_file(&file).unwrap();
        let second = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();

        // Then: the sync takes the delete branch — one file removed, listed in
        // changed_paths, and the store no longer tracks it.
        assert_eq!(second.files_removed, 1);
        assert!(second.changed_paths.contains(&"src/app.ts".to_string()));
        let store = Store::open(&db).unwrap();
        assert!(
            store.file_by_path("src/app.ts").unwrap().is_none(),
            "deleted file must be dropped from the store"
        );
    }

    type RelevantEdgeRows = BTreeMap<String, (i64, String)>;

    fn dependent_ref_narrowing_fixture(name: &str) -> (TestDir, PathBuf, String) {
        let dir = TestDir::new(name);
        fs::create_dir_all(dir.path().join("src")).unwrap();

        let hub_source = (0..20)
            .map(|index| format!("export function hub_{index:02}(): void {{}}\n"))
            .collect::<String>();
        let mut consumer_source = String::from("export function consume(): void {\n");
        for index in 0..19 {
            consumer_source.push_str(&format!("  hub_{index:02}();\n"));
        }
        for index in 0..1_384 {
            consumer_source.push_str(&format!("  missing_{index:04}();\n"));
        }
        consumer_source.push_str("}\n");

        fs::write(dir.path().join("src/hub.ts"), &hub_source).unwrap();
        fs::write(dir.path().join("src/consumer.ts"), consumer_source).unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/hub.ts", "src/consumer.ts"]).unwrap();

        (dir, db, hub_source)
    }

    fn relevant_edge_rows(db: &Path) -> RelevantEdgeRows {
        let store = Store::open(db).unwrap();
        let mut stmt = store
            .connection()
            .prepare(
                r#"SELECT tgt.name, e.id, e.target
                FROM edges e
                JOIN nodes src ON src.id = e.source
                JOIN nodes tgt ON tgt.id = e.target
                WHERE src.file_path = 'src/consumer.ts'
                  AND tgt.file_path = 'src/hub.ts'
                  AND e.kind = 'calls'
                ORDER BY tgt.name"#,
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, String>(2)?),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    fn irrelevant_unresolved_rows(db: &Path) -> Vec<(String, i64)> {
        let store = Store::open(db).unwrap();
        let mut stmt = store
            .connection()
            .prepare(
                r#"SELECT reference_name, id
                FROM unresolved_refs
                WHERE file_path = 'src/consumer.ts'
                  AND reference_name LIKE 'missing_%'
                ORDER BY reference_name, id"#,
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn assert_relevant_edges_were_rebuilt(before: &RelevantEdgeRows, after: &RelevantEdgeRows) {
        assert_eq!(before.len(), 19, "fixture must resolve 19 hub calls");
        assert_eq!(after.len(), 19, "sync must rebuild all 19 hub calls");
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>(),
            "sync must preserve the set of relevant call names"
        );
        for (name, (before_id, _)) in before {
            let (after_id, _) = after.get(name).unwrap();
            assert_ne!(
                after_id, before_id,
                "relevant edge {name} must be rebuilt after its target file changes"
            );
        }
    }

    fn assert_irrelevant_rows_are_identical(before: &[(String, i64)], after: &[(String, i64)]) {
        assert_eq!(before.len(), 1_384, "fixture must park 1384 missing calls");
        assert_eq!(
            after.len(),
            before.len(),
            "sync must retain every unrelated unresolved ref"
        );
        for ((before_name, before_id), (after_name, after_id)) in before.iter().zip(after) {
            assert_eq!(
                after_name, before_name,
                "unrelated unresolved refs must retain their deterministic order"
            );
            assert_eq!(
                after_id, before_id,
                "unrelated unresolved row {before_name} must retain row identity"
            );
        }
    }

    #[test]
    fn dependent_ref_narrowing_tail_edit_rebuilds_only_relevant_sites() {
        let (dir, db, hub_source) = dependent_ref_narrowing_fixture("watch-ref-narrow-tail");
        let edges_before = relevant_edge_rows(&db);
        let irrelevant_before = irrelevant_unresolved_rows(&db);

        fs::write(
            dir.path().join("src/hub.ts"),
            format!("{hub_source}// tail-only edit\n"),
        )
        .unwrap();
        sync_changed_paths(dir.path(), &db, ["src/hub.ts"]).unwrap();

        let edges_after = relevant_edge_rows(&db);
        let irrelevant_after = irrelevant_unresolved_rows(&db);
        assert_relevant_edges_were_rebuilt(&edges_before, &edges_after);
        for (name, (_, target_before)) in &edges_before {
            assert_eq!(
                &edges_after.get(name).unwrap().1,
                target_before,
                "tail edit must not change target node identity for {name}"
            );
        }
        assert_irrelevant_rows_are_identical(&irrelevant_before, &irrelevant_after);
    }

    #[test]
    fn dependent_ref_narrowing_head_edit_retargets_shifted_nodes_only() {
        let (dir, db, hub_source) = dependent_ref_narrowing_fixture("watch-ref-narrow-head");
        let edges_before = relevant_edge_rows(&db);
        let irrelevant_before = irrelevant_unresolved_rows(&db);

        fs::write(
            dir.path().join("src/hub.ts"),
            format!("// head edit shifts every hub node id\n{hub_source}"),
        )
        .unwrap();
        sync_changed_paths(dir.path(), &db, ["src/hub.ts"]).unwrap();

        let edges_after = relevant_edge_rows(&db);
        let irrelevant_after = irrelevant_unresolved_rows(&db);
        assert_relevant_edges_were_rebuilt(&edges_before, &edges_after);

        let store = Store::open(&db).unwrap();
        let new_targets = store
            .nodes_by_file_path("src/hub.ts")
            .unwrap()
            .into_iter()
            .map(|node| (node.name, node.id))
            .collect::<BTreeMap<_, _>>();
        let old_targets = edges_before
            .values()
            .map(|(_, target)| target.clone())
            .collect::<BTreeSet<_>>();
        for (name, (_, target_after)) in &edges_after {
            assert_eq!(
                new_targets.get(name),
                Some(target_after),
                "head edit must retarget {name} to the shifted node id"
            );
            assert!(
                !old_targets.contains(target_after),
                "head edit must not leave {name} pointing at an old target id"
            );
        }
        for old_target in old_targets {
            let count = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM nodes WHERE id = ?1",
                    [old_target],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(count, 0, "old shifted target nodes must be absent");
        }
        assert_irrelevant_rows_are_identical(&irrelevant_before, &irrelevant_after);
    }

    #[test]
    fn dependent_file_is_re_resolved_when_its_import_target_changes() {
        // Given: a helper module and a consumer that imports it, both indexed.
        let dir = TestDir::new("watch-dependents");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/helper.ts"),
            "export function help() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/consumer.ts"),
            "import { help } from './helper';\nexport function use() { return help(); }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/helper.ts", "src/consumer.ts"]).unwrap();

        // When: only the helper changes (adds a symbol) and is re-synced alone,
        // driving the dependent-refresh path for the untouched consumer.
        fs::write(
            dir.path().join("src/helper.ts"),
            "export function help() { return 2; }\nexport function extra() { return 3; }\n",
        )
        .unwrap();
        let outcome = sync_changed_paths(dir.path(), &db, ["src/helper.ts"]).unwrap();

        // Then: the helper is reindexed and the sync completes without error,
        // having walked the dependents/refresh machinery.
        assert_eq!(outcome.files_reindexed, 1);
        assert_eq!(outcome.changed_paths, vec!["src/helper.ts".to_string()]);
    }

    #[test]
    fn framework_sweep_does_not_grow_unresolved_refs_across_heal_cycles() {
        // Given: an indexed Godot project (autoload + signal-handler refs produce
        // framework `unresolved_refs`), with the #1187 marker set so every sync
        // triggers the orphan sweep.
        let dir = TestDir::new("watch-godot-sweep");
        fs::create_dir_all(dir.path().join(".codegraph")).unwrap();
        fs::write(
            dir.path().join("project.godot"),
            "config_version=5\n\n[application]\n\nconfig/name=\"Sweep Fixture\"\n\n[autoload]\n\nGameFlow=\"*res://game_flow.gd\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("game_flow.gd"),
            "extends Node\n\nfunc return_to_map() -> void:\n\tpass\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("stage_manager.gd"),
            "extends Node\n\nfunc _ready() -> void:\n\tvar button := Button.new()\n\tbutton.pressed.connect(_on_pressed.bind(button))\n\nfunc _goto_map() -> void:\n\tGameFlow.return_to_map()\n\nfunc _on_pressed(source) -> void:\n\tpass\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_project_once(dir.path()).unwrap();

        let baseline = {
            let store = Store::open(&db).unwrap();
            store.set_resolution_incomplete().unwrap();
            store.unresolved_refs_count().unwrap()
        };

        // When: two bare no-change syncs run the sweep back-to-back.
        sync_project_once(dir.path()).unwrap();
        let after_first = Store::open(&db).unwrap().unresolved_refs_count().unwrap();
        sync_project_once(dir.path()).unwrap();
        let after_second = Store::open(&db).unwrap().unresolved_refs_count().unwrap();

        // Then: the row count never grows — the sweep re-resolves the existing
        // framework refs instead of re-injecting duplicate ones.
        assert_eq!(
            after_first, baseline,
            "first sweep must not grow unresolved_refs (no duplicate framework rows)"
        );
        assert_eq!(
            after_second, baseline,
            "repeated sweeps must stay idempotent (no accumulation across heal cycles)"
        );
    }

    #[test]
    fn sync_project_once_scans_and_removes_absent_tracked_files() {
        // Given: a project with two source files indexed via a full scan.
        let dir = TestDir::new("watch-once");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".codegraph")).unwrap();
        fs::write(
            dir.path().join("src/a.ts"),
            "export function a() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/b.ts"),
            "export function b() { return 2; }\n",
        )
        .unwrap();
        let first = sync_project_once(dir.path()).unwrap();
        assert!(
            first.files_reindexed >= 2,
            "both files indexed on first scan"
        );

        // When: one file is deleted and a full cold sync runs again — the
        // deletion is discovered by diffing tracked files against the scan set.
        fs::remove_file(dir.path().join("src/b.ts")).unwrap();
        let second = sync_project_once(dir.path()).unwrap();

        // Then: the absent tracked file is removed and the surviving file is
        // skipped as unchanged.
        assert_eq!(second.files_removed, 1);
        assert!(second.changed_paths.contains(&"src/b.ts".to_string()));
    }

    #[test]
    fn sync_project_once_with_progress_reports_monotonic_progress() {
        // Given: a project with a couple of source files.
        let dir = TestDir::new("watch-progress");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".codegraph")).unwrap();
        fs::write(
            dir.path().join("src/a.ts"),
            "export function a() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/b.ts"),
            "export function b() { return 2; }\n",
        )
        .unwrap();

        // When: a progress-reporting sync runs, recording each (done, total).
        let updates = std::cell::RefCell::new(Vec::new());
        sync_project_once_with_progress(dir.path(), |done, total| {
            updates.borrow_mut().push((done, total));
        })
        .unwrap();

        // Then: progress is non-empty, done never exceeds total, and the final
        // done equals total (every candidate was reported).
        let updates = updates.into_inner();
        assert!(!updates.is_empty(), "progress callback must fire");
        let total = updates[0].1;
        assert!(updates.iter().all(|(done, t)| *done <= *t && *t == total));
        assert_eq!(updates.last().unwrap().0, total);
    }

    #[test]
    fn node_names_in_file_returns_stored_symbol_names() {
        // Given: an indexed source file with a named export.
        let dir = TestDir::new("watch-node-names");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();

        // When: the stored node names for that file are queried.
        let store = Store::open(&db).unwrap();
        let names = node_names_in_file(&store, "src/app.ts").unwrap();

        // Then: the exported symbol name is present.
        assert!(
            names.contains("answer"),
            "stored node names should include the exported symbol, got {names:?}"
        );
    }

    #[test]
    fn candidate_changes_ignore_a_move_and_catch_everything_else() {
        use codegraph_core::types::{Language, NodeKind};

        // Given: one `add` function, described by its id, line and export flag.
        let node = |id: &str, line: i64, exported: bool| Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: "add".to_string(),
            qualified_name: "add".to_string(),
            file_path: "src/math.ts".to_string(),
            language: Language::TypeScript,
            start_line: line,
            end_line: line + 2,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: exported,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: line,
        };
        let changed = |before: &[Node], after: &[Node]| {
            let mut names = ChangedNames::default();
            names.note_candidates(before, after);
            assert!(names.candidates.is_subset(&names.any));
            names.candidates
        };

        // Then: a move (new id, lines and timestamp, nothing else) is no candidate change.
        assert!(
            changed(
                &[node("function:a", 1, true)],
                &[node("function:b", 9, true)]
            )
            .is_empty()
        );
        // And: an `export` added on the same line keeps the id and still counts.
        assert!(
            changed(
                &[node("function:a", 1, false)],
                &[node("function:a", 1, true)]
            )
            .contains("add")
        );
        // And: a node arriving counts, and so does one of two identical nodes leaving.
        assert!(changed(&[], &[node("function:a", 1, true)]).contains("add"));
        assert!(
            changed(
                &[node("function:a", 1, true), node("function:b", 9, true)],
                &[node("function:a", 1, true)],
            )
            .contains("add")
        );
    }

    #[test]
    fn interrupted_index_heals_on_bare_sync_and_clears_marker() {
        use codegraph_core::types::{EdgeKind, Language, UnresolvedRef};

        // Given: an index whose full-resolution pass was interrupted — a
        // genuinely-resolvable ref is parked in unresolved_refs with no edge, and
        // the #1187 incomplete-resolution marker is set (the crash signature).
        let dir = TestDir::new("watch-heal-orphan");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/math.ts"),
            "export function add(a: number, b: number): number { return a + b; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "import { add } from './math';\nexport function run(): number { return add(1, 2); }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();

        // Index both files so nodes exist, then simulate the interruption by
        // deleting the resolved Calls edge, re-parking its unresolved ref, and
        // setting the marker.
        sync_changed_paths(dir.path(), &db, ["src/math.ts", "src/app.ts"]).unwrap();
        let caller_id = {
            let store = Store::open(&db).unwrap();
            store
                .nodes_by_file_path("src/app.ts")
                .unwrap()
                .into_iter()
                .find(|n| n.name == "run")
                .expect("run node")
                .id
        };
        {
            let mut store = Store::open(&db).unwrap();
            store.delete_resolved_edges_from_file("src/app.ts").unwrap();
            store
                .insert_unresolved_refs(&[UnresolvedRef {
                    id: None,
                    from_node_id: caller_id.clone(),
                    reference_name: "add".to_string(),
                    reference_kind: EdgeKind::Calls,
                    line: 2,
                    col: 0,
                    candidates: None,
                    file_path: "src/app.ts".to_string(),
                    language: Language::TypeScript,
                    is_function_ref: false,
                    reference_subkind: None,
                }])
                .unwrap();
            store.set_resolution_incomplete().unwrap();
        }

        // When: a bare no-change sync runs (no path is dirty on disk).
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<String>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "no file changed");
        // The sweep's work is reported, so a recovering sync is not a no-op
        // (upstream #1360).
        assert_eq!(
            (
                outcome.pending_refs_processed,
                outcome.pending_refs_resolved,
                outcome.pending_refs_unresolved
            ),
            (1, 1, 0)
        );

        // Then: the orphaned ref is swept into a Calls edge and the marker clears.
        let store = Store::open(&db).unwrap();
        assert!(
            !store.is_resolution_incomplete().unwrap(),
            "sweep must clear the #1187 marker"
        );
        let edges = store
            .edges_by_source_kind(&caller_id, Some(EdgeKind::Calls))
            .unwrap();
        assert!(
            !edges.is_empty(),
            "the orphaned add() call edge must be recovered by the sweep"
        );
    }

    #[test]
    fn healthy_sync_leaves_marker_absent_and_unresolvable_rows_parked() {
        // Given: a healthy index (no marker) with a legitimately-unresolvable ref
        // to an external symbol — Rust retains such rows as #1240 retry state.
        let dir = TestDir::new("watch-healthy-noheal");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function run(): void { externalMissing(); }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();

        // When: a bare no-change sync runs.
        let before = {
            let store = Store::open(&db).unwrap();
            (
                store.is_resolution_incomplete().unwrap(),
                store.unresolved_refs_count().unwrap(),
            )
        };
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<String>::new()).unwrap();

        // Then: no marker was set, nothing was swept, and the retained
        // unresolvable rows are untouched (#1240 retry state preserved).
        assert_eq!(outcome.files_reindexed, 0);
        assert_eq!(outcome.pending_refs_processed, 0, "no sweep ran");
        assert!(!before.0, "a healthy index must not carry the marker");
        let store = Store::open(&db).unwrap();
        assert!(!store.is_resolution_incomplete().unwrap());
        assert_eq!(
            store.unresolved_refs_count().unwrap(),
            before.1,
            "unresolvable retry rows must survive a healthy sync"
        );
    }

    /// Write this project's CURRENT-ROOT `config.toml` — the only config a sync
    /// consults.
    fn write_project_config(project_root: &Path, contents: &str) {
        let paths = index_paths(project_root).expect("resolve project paths");
        fs::create_dir_all(paths.current_root()).unwrap();
        fs::write(paths.config_toml(), contents).unwrap();
    }

    #[test]
    fn sync_project_once_indexes_gitignored_dir_named_in_include() {
        // #1063: a gitignored first-party dir named in `[indexing] include` is
        // indexed on a full `codegraph sync`. The config is THIS project's own
        // current-root `config.toml`, loaded per operation, so the assertion is
        // unconditional — no process-global singleton can hold another project's
        // settings.
        let dir = TestDir::new("watch-once-include");
        fs::write(dir.path().join(".gitignore"), "Tools/\n").unwrap();
        write_project_config(
            dir.path(),
            "[app]\nname = \"p\"\n\n[indexing]\ninclude = [\"Tools/\"]\n",
        );
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("Tools")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function a() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Tools/helper.ts"),
            "export function help() { return 2; }\n",
        )
        .unwrap();

        let outcome = sync_project_once(dir.path()).unwrap();
        assert!(
            outcome
                .changed_paths
                .contains(&"Tools/helper.ts".to_string())
        );
        let store = Store::open(&default_db_path(dir.path()).unwrap()).unwrap();
        assert!(
            store.file_by_path("Tools/helper.ts").unwrap().is_some(),
            "gitignored Tools/ named in include must be indexed on sync"
        );
    }

    /// Two projects synced by ONE process use their OWN current-root configs:
    /// only the project whose config names `Tools/` indexes its gitignored dir.
    #[test]
    fn two_projects_in_one_process_use_their_own_configs() {
        let alpha = TestDir::new("watch-scope-alpha");
        let beta = TestDir::new("watch-scope-beta");
        for project in [alpha.path(), beta.path()] {
            fs::write(project.join(".gitignore"), "Tools/\n").unwrap();
            fs::create_dir_all(project.join("Tools")).unwrap();
            fs::write(
                project.join("Tools/helper.ts"),
                "export function help() { return 2; }\n",
            )
            .unwrap();
        }
        write_project_config(
            alpha.path(),
            "[app]\nname = \"alpha\"\n\n[indexing]\ninclude = [\"Tools/\"]\n",
        );
        write_project_config(beta.path(), "[app]\nname = \"beta\"\n");

        let alpha_outcome = sync_project_once(alpha.path()).unwrap();
        let beta_outcome = sync_project_once(beta.path()).unwrap();
        assert!(
            alpha_outcome
                .changed_paths
                .contains(&"Tools/helper.ts".to_string()),
            "alpha's own include must apply: {alpha_outcome:?}"
        );
        assert!(
            !beta_outcome
                .changed_paths
                .contains(&"Tools/helper.ts".to_string()),
            "beta must not inherit alpha's include: {beta_outcome:?}"
        );
    }

    /// A project-declared custom extension is indexed by a sync AND handled by
    /// the incremental path, proving the current-root `codegraph.json` overrides
    /// reach both scan and watcher gating.
    #[test]
    fn project_extension_overrides_reach_scan_and_incremental_sync() {
        let dir = TestDir::new("watch-ext-override");
        let paths = index_paths(dir.path()).expect("resolve project paths");
        fs::create_dir_all(paths.current_root()).unwrap();
        fs::write(
            paths.extension_config(),
            "{\"extensions\":{\".myext\":\"lua\"}}",
        )
        .unwrap();
        fs::write(dir.path().join("plugin.myext"), "local x = 1\n").unwrap();

        let full = sync_project_once(dir.path()).unwrap();
        assert!(
            full.changed_paths.contains(&"plugin.myext".to_string()),
            "a project-declared extension must be indexed: {full:?}"
        );

        fs::write(dir.path().join("plugin.myext"), "local x = 2\n").unwrap();
        let incremental = sync_changed_paths(
            dir.path(),
            default_db_path(dir.path()).unwrap(),
            ["plugin.myext"],
        )
        .unwrap();
        assert_eq!(
            incremental.files_reindexed, 1,
            "the incremental path must handle it too: {incremental:?}"
        );
    }

    #[test]
    fn sync_changed_paths_reloads_include_for_gitignored_file() {
        // #1063 plus hot reload: each incremental sync reloads the project's
        // current config, so adding an include rule makes a previously ignored
        // gitignored file eligible without restarting the watcher.
        let dir = TestDir::new("watch-incr-include");
        fs::write(dir.path().join(".gitignore"), "Tools/\n").unwrap();
        fs::create_dir_all(dir.path().join("Tools")).unwrap();
        fs::write(
            dir.path().join("Tools/helper.ts"),
            "export function help() { return 1; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();

        // Without include: the gitignored file is ignored by the policy gate.
        let plain = sync_changed_paths(dir.path(), &db, ["Tools/helper.ts"]).unwrap();
        assert_eq!(plain.files_reindexed, 0);
        assert!(plain.files_ignored >= 1);

        write_project_config(
            dir.path(),
            "[app]\nname = \"p\"\n\n[indexing]\ninclude = [\"Tools/\"]\n",
        );
        // With the newly loaded include: the same file is handled and reindexed.
        let included = sync_changed_paths(dir.path(), &db, ["Tools/helper.ts"]).unwrap();
        assert_eq!(included.files_reindexed, 1);
        assert!(
            included
                .changed_paths
                .contains(&"Tools/helper.ts".to_string())
        );
    }

    #[test]
    fn incremental_sync_removes_newly_excluded_file_and_can_readmit_it() {
        let dir = TestDir::new("watch-incr-scope-reload");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function before() { return 1; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();

        let initial = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(initial.files_reindexed, 1);

        write_project_config(
            dir.path(),
            "[app]\nname = \"p\"\n\n[indexing]\nexclude = [\"src/app.ts\"]\n",
        );
        let excluded = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(excluded.files_removed, 1);
        assert!(
            Store::open(&db)
                .unwrap()
                .file_by_path("src/app.ts")
                .unwrap()
                .is_none(),
            "a queued event must not re-admit a file excluded by the current scope"
        );

        write_project_config(dir.path(), "[app]\nname = \"p\"\n");
        let readmitted = sync_changed_paths(dir.path(), &db, ["src/app.ts"]).unwrap();
        assert_eq!(readmitted.files_reindexed, 1);
        assert!(
            Store::open(&db)
                .unwrap()
                .file_by_path("src/app.ts")
                .unwrap()
                .is_some(),
            "removing the exclusion must make the same on-disk file indexable again"
        );
    }

    /// Rewrite the authoritative state slot's built version so the namespace
    /// classifies `Outdated` while its database stays byte-identical.
    fn stage_outdated(dir: &TestDir) -> u64 {
        let paths = index_paths(dir.path()).unwrap();
        let built = codegraph_store::CURRENT_EXTRACTION_VERSION - 1;
        let identity = paths.project_identity();
        let checksum = codegraph_store::checksum_hex(
            77,
            codegraph_store::CURRENT_STORAGE_PROTOCOL,
            built,
            "current",
            identity,
        );
        let body = format!(
            "{{\"sequence\":77,\"storageProtocol\":{},\"extractionVersion\":{built},\
             \"phase\":\"current\",\"projectIdentity\":\"{identity}\",\"checksum\":\"{checksum}\"}}\n",
            codegraph_store::CURRENT_STORAGE_PROTOCOL,
        );
        let [slot0, slot1] = paths.state_slots();
        fs::write(&slot0, body).unwrap();
        let _ = fs::remove_file(&slot1);
        assert_eq!(
            Store::extraction_status(&paths),
            codegraph_store::ExtractionStatus::Outdated { built }
        );
        built
    }

    #[test]
    fn incremental_sync_writes_the_generated_flag_both_ways() {
        // Given: an indexed hand-written Go file whose row reads 0.
        let dir = TestDir::new("watch-generated-incremental");
        let target = dir.path().join("payroll.go");
        fs::write(
            &target,
            "package payroll\n\nfunc ComputePay() int64 { return 1 }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["payroll.go"]).unwrap();
        assert!(!read_generated(dir.path(), "payroll.go"));

        // When: an edit ADDS a generation banner and the incremental sync runs.
        fs::write(
            &target,
            "// Code generated by fkit-crud. DO NOT EDIT.\n\npackage payroll\n\nfunc ComputePay() int64 { return 1 }\n",
        )
        .unwrap();
        sync_changed_paths(dir.path(), &db, ["payroll.go"]).unwrap();

        // Then: the stored verdict flips 0 -> 1 through the real sync path.
        assert!(
            read_generated(dir.path(), "payroll.go"),
            "adding a banner must flip the stored flag to 1"
        );

        // And removing the banner flips it BACK, which re-tests the
        // `DO UPDATE SET` half through sync rather than through the store test.
        fs::write(
            &target,
            "package payroll\n\nfunc ComputePay() int64 { return 2 }\n",
        )
        .unwrap();
        sync_changed_paths(dir.path(), &db, ["payroll.go"]).unwrap();
        assert!(
            !read_generated(dir.path(), "payroll.go"),
            "removing the banner must flip the stored flag back to 0"
        );
    }

    #[test]
    fn outdated_rebuild_writes_the_generated_flag() {
        // Given: a banner-marked file and a hand-written sibling, indexed and then
        // marked Outdated so the next sync escalates into `migrate_project`.
        let dir = TestDir::new("watch-generated-migrate");
        fs::write(
            dir.path().join("payroll.go"),
            "// Code generated by fkit-crud. DO NOT EDIT.\n\npackage payroll\n\nfunc ComputePay() int64 { return 1 }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("payroll_usecase.go"),
            "package payroll\n\nfunc ComputeNet() int64 { return 2 }\n",
        )
        .unwrap();
        sync_project_once(dir.path()).unwrap();
        stage_outdated(&dir);

        // When: the cold full sync runs, which is the path an existing index
        // actually takes when it upgrades (migrate.rs, NOT main.rs).
        sync_project_once(dir.path()).unwrap();

        // Then: the rebuild wrote the real verdict, not a hardcoded false. This is
        // the assertion that fails if write path 3 is left filling `false`.
        assert!(
            read_generated(dir.path(), "payroll.go"),
            "the Outdated rebuild must write generated = 1 for a banner-marked file"
        );
        assert!(
            !read_generated(dir.path(), "payroll_usecase.go"),
            "the Outdated rebuild must leave a hand-written file at 0"
        );
    }

    fn read_generated(project_root: &Path, relative: &str) -> bool {
        let paths = index_paths(project_root).unwrap();
        let store =
            Store::open_for_read(&paths, Instant::now() + Duration::from_secs(10), || false)
                .unwrap();
        store
            .file_by_path(relative)
            .unwrap()
            .unwrap_or_else(|| panic!("{relative} must have a file row"))
            .generated
    }

    #[test]
    fn watcher_sync_on_outdated_namespace_migrates_every_file_without_skips() {
        // Given: an indexed project whose namespace is then marked as built by an
        // OLDER extraction version, with every source byte unchanged.
        let dir = TestDir::new("watch-outdated-migrate");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/alpha.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/beta.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        let first = sync_changed_paths(dir.path(), &db, ["src/alpha.ts", "src/beta.ts"]).unwrap();
        assert_eq!(first.files_reindexed, 2);
        stage_outdated(&dir);

        // When: the watcher's incremental entry point runs for ONE changed path.
        let outcome = sync_changed_paths(dir.path(), &db, ["src/alpha.ts"]).unwrap();

        // Then: it escalated to a full migration — every candidate was processed,
        // nothing was skipped as unchanged, changed_paths is the sorted full set,
        // and the namespace is a readable Current again.
        assert_eq!(outcome.files_skipped_unchanged, 0);
        assert_eq!(outcome.files_reindexed, 2);
        assert_eq!(
            outcome.changed_paths,
            vec!["src/alpha.ts".to_string(), "src/beta.ts".to_string()]
        );
        let paths = index_paths(dir.path()).unwrap();
        assert_eq!(
            Store::extraction_status(&paths),
            codegraph_store::ExtractionStatus::Current
        );
        let store =
            Store::open_for_read(&paths, Instant::now() + Duration::from_secs(10), || false)
                .unwrap();
        assert!(store.file_by_path("src/beta.ts").unwrap().is_some());
    }

    #[test]
    fn full_sync_on_outdated_namespace_migrates_and_drops_absent_files() {
        // Given: two indexed files, one of which is then deleted, over a
        // namespace marked Outdated.
        let dir = TestDir::new("watch-outdated-full");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/alpha.ts"),
            "export function alpha() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/beta.ts"),
            "export function beta() { return 2; }\n",
        )
        .unwrap();
        sync_project_once(dir.path()).unwrap();
        fs::remove_file(dir.path().join("src/beta.ts")).unwrap();
        stage_outdated(&dir);

        // When: the cold full sync runs.
        let outcome = sync_project_once(dir.path()).unwrap();

        // Then: migration re-extracted the surviving file with no skips and the
        // absent file is gone from the fresh database.
        assert_eq!(outcome.files_skipped_unchanged, 0);
        assert_eq!(outcome.changed_paths, vec!["src/alpha.ts".to_string()]);
        let paths = index_paths(dir.path()).unwrap();
        let store =
            Store::open_for_read(&paths, Instant::now() + Duration::from_secs(10), || false)
                .unwrap();
        assert!(store.file_by_path("src/beta.ts").unwrap().is_none());
    }

    #[test]
    fn incremental_sync_rejects_a_database_outside_the_resolved_namespace() {
        // Given: an initialized project and a database path that is NOT its
        // resolved database.
        let dir = TestDir::new("watch-foreign-db");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function app() { return 1; }\n",
        )
        .unwrap();
        let foreign = dir.path().join("elsewhere.db");

        // When/Then: the sync refuses rather than mutating an unclassified file.
        let error = sync_changed_paths(dir.path(), &foreign, ["src/app.ts"]).unwrap_err();
        assert!(
            error.to_string().contains("is not the resolved database"),
            "unexpected error: {error}"
        );
        assert!(
            !foreign.exists(),
            "a refused sync must not create the foreign database"
        );
    }

    #[test]
    fn deleting_an_imported_module_refreshes_its_dependents() {
        // Given: a helper module and a consumer that resolves an import to it,
        // both indexed so a resolved cross-file edge exists.
        let dir = TestDir::new("watch-delete-dependents");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/helper.ts"),
            "export function help() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/consumer.ts"),
            "import { help } from './helper';\nexport function use() { return help(); }\n",
        )
        .unwrap();
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/helper.ts", "src/consumer.ts"]).unwrap();

        // When: the helper is deleted and re-synced, driving the delete branch
        // which gathers dependents and refreshes the surviving consumer's refs.
        fs::remove_file(dir.path().join("src/helper.ts")).unwrap();
        let outcome = sync_changed_paths(dir.path(), &db, ["src/helper.ts"]).unwrap();

        // Then: the helper is removed and the sync completes, having walked the
        // dependent-refresh machinery for the untouched consumer.
        assert_eq!(outcome.files_removed, 1);
        assert!(outcome.changed_paths.contains(&"src/helper.ts".to_string()));
        let store = Store::open(&db).unwrap();
        assert!(
            store.file_by_path("src/consumer.ts").unwrap().is_some(),
            "the consumer must survive the helper's deletion"
        );
    }
}
