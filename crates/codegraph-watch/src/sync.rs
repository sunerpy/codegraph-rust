use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
use codegraph_store::queries::{FileReferenceSite, ReferenceSite, ResolutionReplacement};
use codegraph_store::{
    ExtractionStatus, IndexLease, StatePhase, Store, StoreWriteOpen, StoreWritePurpose, classify,
};
use rayon::prelude::*;

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
    /// The control inputs the index records once this sync is done, by
    /// project path (W5-06): the watcher hands their events to the next sync,
    /// though they are no source files. `None` when the sync did not read
    /// them.
    pub control_paths: Option<crate::control_inputs::RecordedInputs>,
    /// Each of `changed_paths` with its file row as this sync left it (`None`
    /// for a removed file), read by the writer before it returned, so a caller
    /// reports on the sync without opening the index again (W5-09d). `None`
    /// when the outcome did not come from a sync that read them.
    pub changed_records: Option<Vec<(String, Option<FileRecord>)>>,
    /// What the synthesis stage did, when this sync ran it: which steps ran
    /// and how many kept their recorded output (W9-00 row 27).
    pub synthesis: Option<Box<codegraph_resolve::synthesis::StageStats>>,
}

/// The largest change log a sync keeps (W9-00 row 27): past it the sync
/// reruns the synthesis stage whole.
fn change_log_cap() -> usize {
    #[cfg(test)]
    if let Some(cap) = tests::CHANGE_LOG_CAP.with(std::cell::Cell::get) {
        return cap;
    }
    codegraph_store::DEFAULT_CHANGE_LOG_CAP
}

/// Whether a sync's synthesis stage may keep the steps whose inputs the sync
/// leaves alone (W9-00 row 27): only when the marker was current when the
/// sync began, and while the change log records every base row it writes. A
/// change that reruns the stage whole turns the log off at once.
struct SynthesisReuse {
    logging: bool,
}

impl SynthesisReuse {
    /// Read the marker and, when it vouches for the stored edges, turn the
    /// change log on, before the sync's first write.
    fn begin(store: &Store) -> Result<Self> {
        let logging = codegraph_resolve::synthesis::marker_is_current(store)?;
        if logging {
            store.begin_change_log(change_log_cap())?;
        }
        Ok(Self { logging })
    }

    /// The stage reruns whole: a bootstrap, a control input, the frameworks,
    /// the Dart manifests or the orphan sweep moved what no row records.
    fn whole(&mut self, store: &Store) -> Result<()> {
        self.end(store)
    }

    /// The base rows the sync moved, the log off: `None` when the stage
    /// reruns whole, the log overflowed, or it was never on.
    fn take(&mut self, store: &Store) -> Result<Option<codegraph_store::ChangeSet>> {
        if !self.logging {
            return Ok(None);
        }
        self.logging = false;
        Ok(store.take_change_log()?)
    }

    fn end(&mut self, store: &Store) -> Result<()> {
        if self.logging {
            store.end_change_log()?;
            self.logging = false;
        }
        Ok(())
    }
}

/// How long each part of one incremental sync took (W9-00a), for
/// `CODEGRAPH_PHASE_TIMINGS`. Kept beside [`SyncOutcome`], which compares as a
/// result. The six parts follow one another, so they sum to the sync.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncTimings {
    /// Taking the index lease, opening the index, and scanning the project.
    pub scan: Duration,
    /// Re-extracting and writing the changed files, and the lanes' change
    /// checks that come before the framework pass.
    pub files: Duration,
    /// The framework per-file extract and cross-file finalization.
    pub frameworks: Duration,
    /// Refreshing the dependents' references, re-resolving, writing the lane
    /// rows and markers, and the orphan sweep.
    pub resolve: Duration,
    /// The synthesis inputs check, and the stage when its marker is away.
    pub synthesis: Duration,
    /// Reading back the changed records, recording the links and the git
    /// state, and finishing the mutation.
    pub finish: Duration,
    /// The synthesis stage's own parts, when it ran.
    pub stage: Option<codegraph_resolve::synthesis::StageTimings>,
}

impl SyncTimings {
    /// The six parts by name, in the order the sync runs them.
    pub fn parts(&self) -> [(&'static str, Duration); 6] {
        [
            ("scan", self.scan),
            ("files", self.files),
            ("frameworks", self.frameworks),
            ("resolve", self.resolve),
            ("synthesis", self.synthesis),
            ("finish", self.finish),
        ]
    }
}

/// The time since the last lap: the parts of [`SyncTimings`].
struct Lap(Instant);

impl Lap {
    fn now() -> Self {
        Self(Instant::now())
    }

    fn take(&mut self) -> Duration {
        let now = Instant::now();
        let lap = now.saturating_duration_since(self.0);
        self.0 = now;
        lap
    }
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
        .map(|(outcome, _)| outcome)
}

/// Like [`sync_project_once`] but invokes `on_progress(done, total)` after each
/// candidate file is processed, letting a caller drive a progress bar. The
/// callback is a pure side effect: it never gates or reorders work, so the
/// result stays byte-equivalent to `index --force`.
pub fn sync_project_once_with_progress(
    project_root: impl AsRef<Path>,
    on_progress: impl FnMut(usize, usize),
) -> Result<SyncOutcome> {
    sync_project_once_with_progress_timed(project_root, on_progress).map(|(outcome, _)| outcome)
}

/// [`sync_project_once_with_progress`], and how long each part of the sync
/// took (W9-00a): `None` when the index was rebuilt instead, as an outdated
/// or interrupted one is.
pub fn sync_project_once_with_progress_timed(
    project_root: impl AsRef<Path>,
    on_progress: impl FnMut(usize, usize),
) -> Result<(SyncOutcome, Option<SyncTimings>)> {
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
) -> Result<(SyncOutcome, Option<SyncTimings>)> {
    let options = &scope.options;
    let include = options.include.clone();
    let exclude = options.exclude.clone();
    let started = Instant::now();
    let mut timings = SyncTimings::default();

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
            timings.scan = started.elapsed();

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
                &mut timings,
            )?;
            let mut tail = Lap::now();
            record_followed_links(&store, &scan.links)?;
            if let Some(capture) = git_capture {
                capture.record(&store, project_root, options, !scan.links.is_empty())?;
            }
            store.finish_current_mutation()?;
            timings.finish += tail.take();
            Ok((outcome, Some(timings)))
        }
        SyncWriter::MigrationRequired(authorization) => crate::migrate::migrate_project(
            project_root,
            paths,
            authorization,
            scope,
            started,
            &mut on_progress,
        )
        .map(|outcome| (outcome, None)),
    }
}

/// What [`upgrade_outdated_index_once`] found under its exclusive lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutdatedUpgrade {
    /// A complete index an older extraction version built, rebuilt from source
    /// into the current namespace.
    Upgraded(SyncOutcome),
    /// Any other namespace, left untouched; the text says why.
    Skipped(String),
}

/// Rebuild a complete index an older extraction version built, and nothing
/// else: the upgrade a daemon runs before its startup gate.
///
/// The state gate classifies an older version's `building` and `current`
/// phases as `Outdated` (its `uninitialized` one is `Uninitialized`), and
/// `codegraph sync` migrates both. An automatic upgrade must not undo an older
/// `uninit --force` or recreate a database someone removed, so this takes
/// ONE exclusive lease, reclassifies under it, and requires an `Outdated`
/// status whose authoritative slot records the `current` phase, no
/// uninitialized tombstone, and the database file. Only then does it run the
/// migration `sync` runs, under the same lease, so no writer can change the
/// namespace between the check and the rebuild.
pub fn upgrade_outdated_index_once(project_root: impl AsRef<Path>) -> Result<OutdatedUpgrade> {
    let project_root = project_root.as_ref();
    let paths = index_paths(project_root)?;
    let scope = ProjectScope::load(project_root, &paths)?;
    let started = Instant::now();
    let lease = IndexLease::acquire_exclusive_existing(
        &paths,
        Instant::now() + SYNC_LEASE_TIMEOUT,
        || false,
    )?;
    if let Some(reason) = not_a_complete_outdated_index(&paths)? {
        return Ok(OutdatedUpgrade::Skipped(reason));
    }
    match Store::open_for_write(&paths, lease, StoreWritePurpose::IncrementalSync)? {
        StoreWriteOpen::FullRebuildRequired(authorization) => crate::migrate::migrate_project(
            project_root,
            &paths,
            authorization,
            &scope,
            started,
            |_, _| {},
        )
        .map(OutdatedUpgrade::Upgraded),
        // The check above held this lease, so the gate classifies the same
        // `Outdated` namespace; any other answer is refused, not written.
        _ => bail!(
            "the index state gate did not authorize upgrading {}",
            paths.current_root().display()
        ),
    }
}

/// Why the namespace is not a complete index an older extraction version
/// built, or `None` when it is.
fn not_a_complete_outdated_index(paths: &IndexPaths) -> Result<Option<String>> {
    let classification = classify(paths);
    if !matches!(classification.status(), ExtractionStatus::Outdated { .. }) {
        return Ok(Some(format!("index state is {}", classification.status())));
    }
    let record = classification.authoritative().map(|slot| &slot.record);
    if record.and_then(|record| record.phase) != Some(StatePhase::Current) {
        return Ok(Some(format!(
            "the outdated index records phase {}, not {}",
            record.map_or("unknown", |record| record.phase_raw.as_str()),
            StatePhase::Current.as_wire()
        )));
    }
    match fs::symlink_metadata(paths.tombstone()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Ok(Some("its uninitialized tombstone is present".to_string())),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting {}", paths.tombstone().display()));
        }
    }
    match fs::symlink_metadata(paths.current_db()) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(None),
        Ok(_) => Ok(Some("its database is not a regular file".to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Some("its database is missing".to_string()))
        }
        Err(error) => {
            Err(error).with_context(|| format!("inspecting {}", paths.current_db().display()))
        }
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
                &mut SyncTimings::default(),
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
    timings: &mut SyncTimings,
) -> Result<SyncOutcome> {
    let mut lap = Lap::now();
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
    // Whether the synthesis stage may keep the steps whose inputs this sync
    // leaves alone (W9-00 row 27): the marker, read before any change
    // withdraws it as the lanes' markers are, says the stored edges are the
    // stage's output over the base graph as the sync finds it, and the
    // change log on this, the one write connection, holds every base row
    // the sync writes from here on.
    let mut synthesis_reuse = SynthesisReuse::begin(store)?;
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
    let solidity_bootstrap = crate::solidity_heads::needs_bootstrap(store)?;
    // And for the Rust files writing a path receiver whose type came or
    // went: every Rust file is re-resolved once (W4-RS-03).
    let rust_bootstrap = crate::rust_receivers::needs_bootstrap(store)?;
    // And for the PHP files whose calls walk other files' class heads:
    // every PHP file is re-resolved once (W4-PHP-01).
    let php_bootstrap = crate::php_heads::needs_bootstrap(store)?;
    // And for the calls through a class name whose walk reads other files'
    // class heads: every file such a walk reads is re-resolved once (W5-07).
    let class_heads_bootstrap = crate::class_heads::needs_bootstrap(store)?;
    // And for the PHP, Pascal and CFML references retried by a name in any
    // case: every file of those languages is re-resolved once (W4-PHP-03).
    let case_bootstrap = crate::case_folded::needs_bootstrap(store)?;
    // And for the Lua files whose calls read other files' locals and module
    // exports: every Lua file is re-resolved once (W4-LUA-01). A rebuild for
    // an unreadable row starts with the marker current, and a sync with
    // nothing changed never withdraws it: withdraw it before anything
    // changes, so one stopping partway leaves the next to rebuild again.
    let lua_bootstrap = crate::lua_modules::needs_bootstrap(store)?;
    if lua_bootstrap {
        crate::lua_modules::withdraw_marker(store)?;
    }
    // And for the R calls of a base function's name whose binding's line a
    // failed sync may have changed: every R file is re-resolved once
    // (W4-R-01).
    let r_bootstrap = crate::r_bindings::needs_bootstrap(store)?;
    // And for the Play routes references whose actions a failed sync's
    // changes moved: every routes file is re-resolved once (W7-19c). The
    // lane compares the changed files' surfaces only when the index recorded
    // Play: otherwise a Play this sync detects is a framework change, which
    // re-extracts every file.
    let play_bootstrap = crate::play_routes::needs_bootstrap(store)?;
    let recorded_frameworks =
        crate::control_inputs::recorded_frameworks(store)?.unwrap_or_default();
    let recorded = |framework: &str| recorded_frameworks.iter().any(|name| name == framework);
    changed_names.play.track = recorded("play");
    // And for the Laravel route references whose controllers a failed
    // sync's changes moved, and the Drupal handlers and hooks (W7-19d):
    // every file holding one is re-resolved once. Each lane compares only
    // when the index recorded its framework, as Play's does.
    let laravel_bootstrap = crate::laravel_routes::needs_bootstrap(store)?;
    changed_names.laravel.track = recorded("laravel");
    let drupal_bootstrap = crate::drupal_refs::needs_bootstrap(store)?;
    changed_names.drupal.track = recorded("drupal");
    // And for the Rails route references whose controllers a failed sync's
    // changes moved (W7-19e), the same way.
    let rails_bootstrap = crate::rails_routes::needs_bootstrap(store)?;
    changed_names.rails.track = recorded("rails");
    // And for the Vapor route handlers whose candidates a failed sync's
    // changes moved (W7-19g), the same way.
    let vapor_bootstrap = crate::vapor_routes::needs_bootstrap(store)?;
    changed_names.vapor.track = recorded("vapor");
    // And for the bridge references a failed sync's changes moved (W7-21):
    // the bridge-facts rows are rebuilt and, in a project a bridge is
    // detected in, every JavaScript-family, Swift and Objective-C file is
    // re-resolved once. The lane compares only when the index recorded a
    // bridge reading the rows, as Play's does.
    let rn_bootstrap = crate::rn_bridges::needs_bootstrap(store)?;
    // And for the module bridges whose wiring a failed sync's changes
    // moved (W7-24 cost, OD-B73): every file holding a `module.M:`
    // reference is re-resolved once and the wiring rows are rebuilt.
    let terraform_bootstrap = crate::terraform_wiring::needs_bootstrap(store)?;
    changed_names.rn_bridges.track = crate::rn_bridges::READERS.iter().any(|name| recorded(name));
    // And for the Rust references the module rule links to a crate root a
    // failed sync's changes added or removed (W7-19h): every Rust file is
    // re-resolved once. The lane retries only when the index recorded Rust,
    // as Play's does.
    let rust_crate_roots_bootstrap = crate::rust_crate_roots::needs_bootstrap(store)?;
    let mut rust_crate_roots_track = recorded("rust");
    // And for the Dart files whose bare calls read other files' class heads:
    // every Dart file is re-resolved once (W4-DART-01). A rebuild for an
    // unreadable row starts with the marker current: withdraw it before
    // anything changes.
    let dart_bootstrap = crate::dart_heads::needs_bootstrap(store)?;
    if dart_bootstrap {
        crate::dart_heads::withdraw_marker(store)?;
    }
    // And for the Dart names and imports that read other files' directives:
    // every Dart file is re-resolved once (W4-DART-02).
    let dart_library_bootstrap = crate::dart_libraries::needs_bootstrap(store)?;
    if dart_library_bootstrap {
        crate::dart_libraries::withdraw_marker(store)?;
    }

    // And for the control files resolution reads (W5-06): every file of the
    // three kinds is re-resolved once. Read here, before a change withdraws
    // the marker.
    let control_bootstrap = crate::control_inputs::needs_bootstrap(store)?;
    // Whether an unchanged dependent's stored references stand in for its
    // extraction this sync (W5-09e), read before a change withdraws the
    // marker.
    let stored_refs_marker =
        crate::resolved_refs::valid_marker(store, &scope.options, &scope.framework)?;

    // A Shopify theme whose marker came or went reclassifies its JSON
    // templates and moves the references its Liquid files make (W4-LIQ-01):
    // the templates join the paths, which the pass below keeps or drops as
    // the scan would.
    let shopify_bootstrap = crate::shopify_themes::needs_bootstrap(store)?;
    let mut paths = paths
        .into_iter()
        .map(|path| path.as_ref().to_path_buf())
        .collect::<Vec<_>>();
    let requested = paths
        .iter()
        .filter_map(|path| policy.normalize_relative(path))
        .collect::<Vec<_>>();
    let shopify = crate::shopify_themes::theme_refresh(
        store,
        project_root,
        requested.iter().map(String::as_str),
        shopify_bootstrap,
    )?;
    paths.extend(shopify.json_paths.iter().map(std::path::PathBuf::from));
    #[cfg(test)]
    if tests::FAIL_AFTER_SHOPIFY_REFRESH.with(std::cell::Cell::get) {
        bail!("injected sync failure after the theme lane's refresh");
    }
    // The project's custom Lua loaders and module roots (#1617), compared
    // with the settings the index records before anything changes, once
    // every marker the first change withdraws is read: new settings
    // withdraw the record first, so a sync stopping partway leaves the next
    // one to extract every Lua file again (W9-02).
    let loaders_change = crate::lua_modules::loaders_change(store, &scope.options.lua_loaders)?;
    if loaders_change != crate::lua_modules::LoadersChange::Same {
        changed_names.begin_mutation(store)?;
        crate::lua_modules::withdraw_loaders(store)?;
    }
    let total = paths.len();
    for (done, path) in paths.into_iter().enumerate() {
        let Some(relative) = policy.normalize_relative(path.as_path()) else {
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
            false,
        )? {
            changed = true;
            reindexed.insert(relative);
        }
        on_progress(done + 1, total);
    }
    // The files whose references into a changed or removed file this sync
    // refreshes at their sites, as the paths left them: a navigation call
    // in one may read its destination through the changed file (W7-23b's
    // N2).
    let extraction_dependents: BTreeSet<String> = dependent_sites.keys().cloned().collect();

    // What resolution reads of the control files (W5-06), compared once the
    // paths are processed and before any lane reads the changes: a kind that
    // moved re-resolves its files whole below, and a framework that came or
    // went re-extracts every file now, so the lanes see those files too.
    let control = crate::control_inputs::check(
        store,
        project_root,
        scanned.is_some(),
        control_bootstrap,
        &requested,
        &changed_names.membership,
    )?;
    if control.any() {
        changed_names.begin_mutation(store)?;
        synthesis_reuse.whole(store)?;
    }
    // The pubspec manifests the Dart library model reads (W5-06), compared
    // with their rows once the paths are processed: when they moved, the
    // Dart lane compares the sync's two states, each with its own.
    let dart_manifests_touched = scanned.is_some()
        || requested
            .iter()
            .any(|path| crate::dart_libraries::is_pubspec_path(path))
        || changed_names
            .added_files
            .iter()
            .chain(&changed_names.removed_files)
            .any(|path| path.ends_with(".dart"));
    let pubspecs =
        crate::dart_libraries::pubspec_check(store, project_root, dart_manifests_touched)?;
    if pubspecs.moved() {
        changed_names.begin_mutation(store)?;
        synthesis_reuse.whole(store)?;
    }
    let any_bootstrap = surface_bootstrap
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
        || solidity_bootstrap
        || rust_bootstrap
        || php_bootstrap
        || class_heads_bootstrap
        || case_bootstrap
        || lua_bootstrap
        || r_bootstrap
        || dart_bootstrap
        || dart_library_bootstrap
        || play_bootstrap
        || laravel_bootstrap
        || drupal_bootstrap
        || rails_bootstrap
        || vapor_bootstrap
        || rn_bootstrap
        || terraform_bootstrap
        || rust_crate_roots_bootstrap
        || shopify_bootstrap
        || !shopify.moved.is_empty();
    // A bootstrap re-resolves dependents without `begin_mutation`: the
    // synthesized edges go stale with them, so their marker goes first
    // (W7-14).
    if any_bootstrap {
        codegraph_resolve::synthesis::withdraw_marker(store)?;
        synthesis_reuse.whole(store)?;
    }
    // A sync with nothing to do detects no framework: the frameworks read
    // their control files (`package.json`, the Tauri markers), which are the
    // JS kind's inputs, and the sources a changed file brings.
    let block = changed
        || any_bootstrap
        || control.any()
        || pubspecs.moved()
        || loaders_change != crate::lua_modules::LoadersChange::Same
        || crate::control_inputs::frameworks_unchecked(store)?;
    let mut resolver = ReferenceResolver::new(project_root.to_string_lossy())
        .with_max_file_size(scope.options.max_file_size)
        .with_lua_loaders(Arc::clone(&scope.options.lua_loaders));
    let mut frameworks = Vec::new();
    let mut frameworks_changed = false;
    if block {
        {
            let context = resolver.store_context(store);
            resolver.initialize(&context);
        }
        frameworks = resolver.framework_signature();
        frameworks_changed = crate::control_inputs::frameworks_changed(store, &frameworks)?;
    }
    if frameworks_changed {
        changed_names.begin_mutation(store)?;
        synthesis_reuse.whole(store)?;
        crate::control_inputs::withdraw_frameworks_marker(store)?;
        // Every file is re-extracted and re-resolved: no lane compares.
        changed_names.play.track = false;
        changed_names.laravel.track = false;
        changed_names.drupal.track = false;
        changed_names.rails.track = false;
        changed_names.vapor.track = false;
        changed_names.rn_bridges.track = false;
        rust_crate_roots_track = false;
        for file in store.all_files()? {
            if reindexed.contains(&file.path) || changed_names.removed_files.contains(&file.path) {
                continue;
            }
            if sync_one(
                project_root,
                store,
                &file.path,
                scope,
                rehash,
                &mut outcome,
                &mut dependent_sites,
                &mut dependent_fallbacks,
                &mut changed_names,
                false,
                true,
            )? {
                reindexed.insert(file.path);
            }
        }
    }
    // A loader call is an import only under the settings that name it
    // (W9-02): when the loader names changed, or the record is missing,
    // every Lua and Luau file is extracted again, before any lane reads
    // the changes. A framework change has re-extracted every file already.
    if matches!(
        loaders_change,
        crate::lua_modules::LoadersChange::Loaders | crate::lua_modules::LoadersChange::Bootstrap
    ) && !frameworks_changed
    {
        let skip =
            |path: &str| reindexed.contains(path) || changed_names.removed_files.contains(path);
        for path in crate::lua_modules::every_lua_file(store, &skip)? {
            if sync_one(
                project_root,
                store,
                &path,
                scope,
                rehash,
                &mut outcome,
                &mut dependent_sites,
                &mut dependent_fallbacks,
                &mut changed_names,
                false,
                true,
            )? {
                reindexed.insert(path);
            }
        }
        #[cfg(test)]
        if tests::FAIL_AFTER_LUA_LOADER_REEXTRACT.with(std::cell::Cell::get) {
            bail!("injected sync failure once every Lua file is extracted again");
        }
    }

    if block {
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
        // A Spring config key binds by its relaxed spelling, and a second
        // profile file's key moves an existing binding's confidence or
        // target, which no name lookup reaches (W7-19c): a changed config
        // file retries every reference a key can bind, the unresolved ones by
        // their whole names, the resolved ones by site.
        if !frameworks_changed
            && frameworks.iter().any(|name| name == "spring")
            && reindexed
                .iter()
                .chain(&changed_names.removed_files)
                .any(|file| codegraph_resolve::frameworks::spring::is_spring_config_file(file))
        {
            changed_names.note_any(store.unresolved_config_key_names()?);
            for found in store.resolved_config_key_sites()? {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
        }
        // What other files read from a changed JavaScript-family module's
        // text no node records (W4a-F3): the files resolving through it, up
        // the barrels that forward it, re-resolve whole.
        let surfaces = crate::module_surfaces::note_surface_changes(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.js_surfaces,
            &reindexed,
            !changed_names.added_files.is_empty()
                || !changed_names.removed_files.is_empty()
                || control
                    .changed
                    .contains(&crate::control_inputs::ControlKind::Js),
            surface_bootstrap,
        )?;
        for file in &surfaces.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(surfaces.names.iter().cloned());
        // W7-31 (OD-B72) — a module re-extracted with its facts unchanged
        // may still import its value's class through a barrel that moved in
        // this sync (the two-hop), and the surface lane re-resolves no
        // re-extracted file: when the lane walked, lane B takes its
        // bindings after all.
        if surfaces.walked() {
            for (module, importers) in std::mem::take(&mut changed_names.quiet_bindings) {
                let into = changed_names.bindings.entry(module).or_default();
                for (importer, sites) in importers {
                    into.entry(importer).or_default().extend(sites);
                }
            }
        }
        // What a Terraform module bridge reads beside its reference: a
        // module declaration's `source` and `component`, a variable's
        // `default`, the names a directory declares, and which `.tf` files
        // the module's directory holds (W7-24c). Only a changed file whose
        // wiring row moved, a `.tf` or `.tofu` file that came or went, or a
        // missing marker re-resolves whole every file holding a `module.M:`
        // reference: one scan of both reference tables, uncapped (W7-24
        // cost, OD-B73).
        let terraform = crate::terraform_wiring::wiring_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.terraform,
            changed_names
                .added_files
                .iter()
                .chain(&changed_names.removed_files)
                .chain(&changed_names.file_nodes_changed)
                .any(|path| crate::terraform_wiring::is_module_entry_candidate(path)),
            terraform_bootstrap,
        )?;
        if terraform.rewire {
            for file in store.terraform_module_reference_files()? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
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
        // And what a bare Solidity call reads of the `is` clauses of the
        // contracts its chain reaches (W4-SOL-01).
        let solidity = crate::solidity_heads::solidity_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.solidity,
            &reindexed,
            &changed_names.removed_files,
            solidity_bootstrap,
        )?;
        for file in &solidity.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for affected in &solidity.sites {
            if !dependent_fallbacks.contains(&affected.file_path) {
                merge_dependent_site(
                    &mut dependent_sites,
                    &mut dependent_fallbacks,
                    affected.clone(),
                );
            }
        }
        // And what a section or snippet reference in a Shopify theme's file
        // reads of the theme's markers (W4-LIQ-01).
        for file in shopify.liquid_files(store)? {
            if !reindexed.contains(&file) {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
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
        // What a call through a class name reads of the heads its walk climbs
        // (upstream #2180, W5-07): the edges to the methods the walk through a
        // changed head can reach re-resolve at their sites, in either file, and
        // the calls parked through a receiver below it are retried.
        let class_heads = crate::class_heads::class_head_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.class_heads,
            &reindexed,
            &changed_names.removed_files,
            class_heads_bootstrap,
        )?;
        for file in &class_heads.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for affected in &class_heads.sites {
            merge_dependent_site(
                &mut dependent_sites,
                &mut dependent_fallbacks,
                affected.clone(),
            );
        }
        let head_target_names: Vec<String> = class_heads.names.iter().cloned().collect();
        for affected in store
            .reference_sites_of_edges_to_named_targets(&head_target_names)?
            .into_iter()
            .chain(store.reference_sites_of_same_file_edges_to_named_targets(&head_target_names)?)
        {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        // What a bare Dart call reads of the class heads of a changed file's
        // types, and a core method name of an extension's `on` type
        // (W4-DART-01).
        let dart = crate::dart_heads::dart_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.dart,
            &reindexed,
            &changed_names.removed_files,
            dart_bootstrap,
        )?;
        for file in &dart.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for affected in &dart.sites {
            merge_dependent_site(
                &mut dependent_sites,
                &mut dependent_fallbacks,
                affected.clone(),
            );
        }
        changed_names.note_all(dart.names.iter().cloned());
        // What a Dart member read or a call on a typed receiver in an
        // unchanged file read of a changed file's heads, and the members it
        // waited for (W4-DART-03). Both ride on the Dart heads marker.
        for affected in crate::dart_heads::typed_receiver_sites(
            store,
            &changed_names.dart,
            &reindexed,
            &changed_names.removed_files,
        )? {
            if !dependent_fallbacks.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        for affected in crate::dart_heads::new_member_sites(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.dart,
            &reindexed,
            &changed_names.removed_files,
        )? {
            if !dependent_fallbacks.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        // What a Dart name or import in an unchanged file read of a changed
        // file's directives: its library's parts, its exports, its imports
        // (W4-DART-02).
        let dart_libraries = crate::dart_libraries::dart_library_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.dart_libraries,
            &changed_names.dart,
            &changed_names.added_files,
            &reindexed,
            &changed_names.removed_files,
            &pubspecs,
            dart_library_bootstrap,
        )?;
        for file in &dart_libraries.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for affected in &dart_libraries.sites {
            if !dependent_fallbacks.contains(&affected.file_path) {
                merge_dependent_site(
                    &mut dependent_sites,
                    &mut dependent_fallbacks,
                    affected.clone(),
                );
            }
        }
        changed_names.note_all(dart_libraries.names.iter().cloned());
        // What a later link of a Dart call chain in an unchanged file read of
        // a changed file's declarations, or of a library's visibility: by the
        // names its file writes, or through a declaration naming one
        // (W4-DART-03b). A bootstrap of the Dart or member rows re-resolves
        // every Dart file already.
        let dart_chains = if dart_bootstrap || member_bootstrap {
            Vec::new()
        } else {
            crate::dart_chains::chain_refresh(
                store,
                project_root,
                scope.options.max_file_size,
                &changed_names.members,
                &dart_libraries.names,
                &changed_names.dart,
                &reindexed,
                &changed_names.removed_files,
            )?
        };
        for affected in dart_chains {
            if !dependent_fallbacks.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        // What a Dart call through an import prefix in an unchanged file
        // waits for: a top-level declaration of its member's name the prefix
        // brings in (W4-DART-03d). A bootstrap of the Dart, member or library
        // rows re-resolves every Dart file already.
        if !(dart_bootstrap || member_bootstrap || dart_library_bootstrap) {
            for affected in crate::dart_libraries::prefixed_call_sites(
                store,
                &changed_names.dart_libraries,
                &changed_names.candidates,
                &reindexed,
                &changed_names.removed_files,
            )? {
                if !dependent_fallbacks.contains(&affected.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
                }
            }
        }
        // What a Lua call in an unchanged file read of a changed one: which
        // names it declares `local`, what its module hands out (W4-LUA-01).
        let lua = crate::lua_modules::lua_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.lua,
            &changed_names.any,
            &reindexed,
            &changed_names.removed_files,
            lua_bootstrap,
        )?;
        for file in &lua.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        changed_names.note_all(lua.names.iter().cloned());
        // A change of the custom Lua module roots alone moves where a Lua
        // import resolves, and what an alias through one names (W9-02):
        // every Lua import goes back by its site, and every Lua file whose
        // aliases `require` a module goes back whole. Nothing is extracted
        // again: the roots decide no extraction.
        if loaders_change == crate::lua_modules::LoadersChange::Roots {
            for found in store.lua_import_sites()? {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            let skip =
                |path: &str| reindexed.contains(path) || changed_names.removed_files.contains(path);
            for file in crate::lua_modules::alias_readers(store, &skip)? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
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
        if r_bootstrap {
            for file in
                crate::r_bindings::every_r_file(store, &reindexed, &changed_names.removed_files)?
            {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if play_bootstrap {
            for file in crate::play_routes::every_routes_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if laravel_bootstrap {
            for file in crate::laravel_routes::every_route_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if drupal_bootstrap {
            for file in crate::drupal_refs::every_reference_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if rails_bootstrap {
            for file in crate::rails_routes::every_route_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if vapor_bootstrap {
            for file in crate::vapor_routes::every_route_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        if case_bootstrap {
            for file in crate::case_folded::every_case_insensitive_file(
                store,
                &reindexed,
                &changed_names.removed_files,
            )? {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
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
        if rust_crate_roots_bootstrap {
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
            let mut reexported = Vec::new();
            for (file, statement) in &changed_names.python_imports {
                reexported.extend(codegraph_resolve::import_resolver::python_reexported_names(
                    file, statement, &context,
                ));
            }
            changed_names.note_any(reexported);
        }
        let name_list: Vec<String> = changed_names.any.iter().cloned().collect();
        for affected in store.reference_sites_of_edges_to_named_targets(&name_list)? {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        // A PHP, Pascal or CFML reference reaches a target by its name in any
        // case (upstream #2141's pre-filter, then the case-insensitive match),
        // so a touched name moves the edges to its namesakes in other cases
        // too, in either file (W4-PHP-03).
        for affected in store.reference_sites_of_edges_to_folded_targets(
            &codegraph_resolve::CASE_INSENSITIVE_LANGUAGES,
            &name_list,
        )? {
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
        // W7-27 — the resolved counterpart: such a file can come ahead of a
        // resolved import's answer, which no edge into a changed file and no
        // touched name reaches. Each resolved import those keys can name, in
        // a file this sync did not re-extract, is resolved again over the
        // store the sync has written; when its answer is not the target its
        // edge holds, its file goes whole, since the statement's bindings
        // and the calls through them follow that answer.
        if !path_keys.is_empty() {
            let context = codegraph_resolve::StoreResolutionContext::new(
                store,
                project_root.to_string_lossy(),
            )
            .with_max_file_size(scope.options.max_file_size)
            .with_lua_loaders(Arc::clone(&scope.options.lua_loaders));
            for resolved in store.resolved_imports_naming(&path_keys)? {
                if reindexed.contains(&resolved.file_path)
                    || dependent_fallbacks.contains(&resolved.file_path)
                {
                    continue;
                }
                let view = codegraph_resolve::RefView {
                    row_id: resolved.id,
                    from_node_id: resolved.from_node_id.clone(),
                    reference_name: resolved.reference_name.clone(),
                    reference_kind: resolved.reference_kind,
                    line: resolved.line,
                    column: resolved.col,
                    file_path: resolved.file_path.clone(),
                    language: resolved.language,
                    is_function_ref: resolved.is_function_ref,
                    reference_subkind: resolved.reference_subkind,
                };
                let now: Vec<String> =
                    codegraph_resolve::import_resolver::resolve_via_import(&view, &context)
                        .map(|answer| answer.target_node_id)
                        .into_iter()
                        .collect();
                let held = store.edge_targets_at_site(
                    &resolved.from_node_id,
                    resolved.line,
                    resolved.col,
                    resolved.reference_kind,
                )?;
                if now != held {
                    dependent_sites.remove(&resolved.file_path);
                    dependent_fallbacks.insert(resolved.file_path);
                }
            }
        }
        // W7-27 — and a C or C++ translation unit's include walk takes the
        // first indexed candidate of each include and reads it, so a file
        // that comes or goes beside an include, or a header that gains or
        // loses its nodes across `max_file_size` (its includes and defines
        // with them), can change whether a call in a file reaching it is a
        // macro expansion, which no name touches: the files whose include
        // spells such a file's name or stem, with every file whose include
        // spells one of those in turn, go whole (none when the index holds
        // no function-like macro).
        let came_or_went: Vec<String> = changed_names
            .added_files
            .iter()
            .chain(&changed_names.removed_files)
            .chain(&changed_names.file_nodes_changed)
            .cloned()
            .collect();
        if !came_or_went.is_empty() {
            for file in resolver.include_walk_dependents(store, &came_or_went) {
                if !reindexed.contains(&file) {
                    dependent_sites.remove(&file);
                    dependent_fallbacks.insert(file);
                }
            }
        }
        // W5-06 — a control input that moved re-resolves every file of its
        // kind whole.
        for file in crate::control_inputs::files_of(store, &control.changed)? {
            if !reindexed.contains(&file) {
                dependent_sites.remove(&file);
                dependent_fallbacks.insert(file);
            }
        }
        // #2450 — and so do the importers of an edited module whose statement
        // holds a binding the import did not resolve (read in `sync_one`).
        for importer in std::mem::take(&mut changed_names.binding_importers) {
            if !reindexed.contains(&importer) {
                dependent_fallbacks.insert(importer);
            }
        }
        #[cfg(test)]
        if tests::FAIL_BEFORE_FRAMEWORK_REFS.with(std::cell::Cell::get) {
            bail!("injected sync failure before the changed files' framework references");
        }
        timings.files += lap.take();
        // Re-run framework per-file extract for reindexed files whose framework
        // nodes/refs were dropped on re-extraction (upstream tree-sitter.ts:4796-4819).
        if resolver.has_framework_resolvers() {
            let reindexed_files: Vec<String> = reindexed.iter().cloned().collect();
            let framework_files = resolver.extract_and_persist_frameworks_with(
                store,
                &reindexed_files,
                &scope.framework,
                &scope.options.extensions,
            )?;
            changed_names.note_framework_node_files(&framework_files);
        }
        // Cross-file framework finalization on every sync, before anything is
        // resolved, as a full index runs it (upstream's sync runs
        // runPostExtract before its resolution too): a route a constant names
        // is renamed before a navigation call resolves against it. The
        // updates are compared with the stored nodes before they are written,
        // for the restore check.
        let post_extract = resolver.post_extract_updates(store);
        changed_names.note_post_extract_updates(store, &post_extract)?;
        if !post_extract.is_empty() {
            store.upsert_nodes(&post_extract)?;
        }
        // Then the nodes no single file's extraction makes, the routes of
        // the tables another file hands React Router (W7-09d): what the
        // passes want now, compared with the store, the first change
        // withdrawing the markers. The edges into a node that goes are
        // re-read as an unchanged dependent's sites, and a node that comes
        // has its references resolved below.
        let cross_file = resolver.plan_cross_file_nodes(store)?;
        if cross_file.changes_store() {
            changed_names.begin_mutation(store)?;
            changed_names.abandon_restore_check();
        }
        let cross_file_applied = resolver.apply_cross_file_plan(store, &cross_file, false)?;
        for affected in cross_file_applied.incoming {
            if !reindexed.contains(&affected.file_path) {
                merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, affected);
            }
        }
        timings.frameworks += lap.take();
        // The Play actions whose candidates the changes moved (W7-19c), read
        // once the framework nodes are back: the unresolved ones retry by
        // their whole names, which no tail ceiling bounds, and the resolved
        // ones by site, an edge's target being only the action's method.
        if changed_names.play.track && frameworks.iter().any(|name| name == "play") {
            let moved: Vec<String> = changed_names.play.moved_names(store)?.into_iter().collect();
            if !moved.is_empty() {
                for found in store.resolved_reference_sites_named(&moved)? {
                    if !reindexed.contains(&found.file_path) {
                        merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                    }
                }
                changed_names.note_any(moved);
            }
        }
        // The Laravel controllers and the Drupal handlers and hooks whose
        // candidates the changes moved (W7-19d), read the same way: the
        // unresolved ones by their whole names, the resolved ones by site.
        if changed_names.laravel.track && frameworks.iter().any(|name| name == "laravel") {
            let moved = changed_names.laravel.moved(store)?;
            let retries = crate::laravel_routes::retries(store, &moved)?;
            for found in retries.sites {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            changed_names.note_any(retries.names);
        }
        if changed_names.drupal.track && frameworks.iter().any(|name| name == "drupal") {
            let moved = changed_names.drupal.moved(store)?;
            let retries =
                crate::drupal_refs::retries(store, &moved, changed_names.candidates.iter())?;
            for found in retries.sites {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            changed_names.note_any(retries.names);
        }
        // The Rails route targets whose controllers the changes moved
        // (W7-19e), read the same way.
        if changed_names.rails.track && frameworks.iter().any(|name| name == "rails") {
            let moved = changed_names.rails.moved(store)?;
            let retries = crate::rails_routes::retries(store, &moved)?;
            for found in retries.sites {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            changed_names.note_any(retries.names);
        }
        // The Vapor route handlers whose method a changed file declares on
        // either side (W7-19g), read the same way.
        if changed_names.vapor.track && frameworks.iter().any(|name| name == "vapor") {
            let touched = changed_names.vapor.touched(store)?;
            let retries = crate::vapor_routes::retries(store, &touched)?;
            for found in retries.sites {
                if !reindexed.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            changed_names.note_any(retries.names);
        }
        // The bridge references the changes moved (W7-21), read once the
        // framework nodes are back: the importers of a changed exported
        // `requireNativeModule` binding re-resolve whole, the unresolved
        // references retry by their whole names and the resolved ones by
        // site, a bridge's weak answer among them when a node of its
        // reference's name, of any language, came, went or changed. A
        // bootstrap rebuilds the rows and re-resolves every
        // JavaScript-family, Swift and Objective-C file of a bridged project.
        let detected =
            |names: &[&str]| frameworks.iter().any(|name| names.contains(&name.as_str()));
        let rn_bridges = crate::rn_bridges::bridge_refresh(
            store,
            project_root,
            scope.options.max_file_size,
            &changed_names.rn_bridges,
            &reindexed,
            &changed_names.removed_files,
            rn_bootstrap,
            detected(&crate::rn_bridges::BRIDGES),
            changed_names.rn_bridges.track && detected(&crate::rn_bridges::READERS),
        )?;
        for file in &rn_bridges.files {
            dependent_sites.remove(file);
            dependent_fallbacks.insert(file.clone());
        }
        for found in &rn_bridges.sites {
            if !dependent_fallbacks.contains(&found.file_path) {
                merge_dependent_site(
                    &mut dependent_sites,
                    &mut dependent_fallbacks,
                    found.clone(),
                );
            }
        }
        changed_names.note_any(rn_bridges.names.iter().cloned());
        // The Rust module rule's targets, the crate roots the changes added
        // or removed or whose file node came or went (W7-19h): a target is a
        // file node named for its file, so no name the changes touched
        // reaches the references naming its module or crate. They are read
        // the same way.
        if rust_crate_roots_track && frameworks.iter().any(|name| name == "rust") {
            let names: Vec<String> = {
                let context = codegraph_resolve::StoreResolutionContext::new(
                    store,
                    project_root.to_string_lossy(),
                )
                .with_max_file_size(scope.options.max_file_size);
                let crates = codegraph_resolve::ResolutionContext::cargo_workspace_crates(&context);
                let resized = changed_names
                    .file_nodes_changed
                    .iter()
                    .filter(|path| crate::rust_crate_roots::may_be_module_target(path));
                crate::rust_crate_roots::names_reaching(
                    changed_names
                        .added_files
                        .iter()
                        .chain(&changed_names.removed_files)
                        .chain(resized),
                    &crates,
                )
                .into_iter()
                .collect()
            };
            if !names.is_empty() {
                for found in store.resolved_reference_sites_named(&names)? {
                    if !reindexed.contains(&found.file_path) {
                        merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                    }
                }
                changed_names.note_any(names);
            }
        }
        // W7-23 — the navigation calls a changed route may answer, and the
        // route and value references a changed module can move, read once
        // the framework nodes are back: a call naming a route by path
        // (#2425), a route's lazily loaded module (#2422), through the
        // barrels that forward it (#2560), and a same-file lazy value a
        // route or a tag renders (#2452). None of them names a symbol the
        // changed file declares or holds an edge into a barrel. They go
        // back by site, with no ceiling. A sync that re-resolves every
        // JavaScript-family file whole already covers them.
        let js_whole = frameworks_changed
            || surface_bootstrap
            || control
                .changed
                .contains(&crate::control_inputs::ControlKind::Js);
        if !js_whole {
            // W7-23b — the navigation calls a route that appeared, went or
            // moved may answer (N1, #2425), and those whose destination a
            // rewritten file may move (N2), before the route-module lanes.
            let rewritten: BTreeSet<String> = reindexed
                .iter()
                .chain(&changed_names.removed_files)
                .cloned()
                .collect();
            let navigation = crate::navigation::navigation_sites(
                store,
                &resolver,
                &crate::navigation::NavigationChanges {
                    routes_before: changed_names.routes_before.as_deref(),
                    rewritten: &rewritten,
                    dependents: &extraction_dependents,
                },
                &reindexed,
            )?;
            for found in navigation {
                if !dependent_fallbacks.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            let mut modules: BTreeSet<String> = BTreeSet::new();
            for file in reindexed.iter().chain(&changed_names.file_nodes_changed) {
                let language = store.file_by_path(file)?.map(|record| record.language);
                if crate::route_modules::may_be_a_module(file, language) {
                    modules.insert(file.clone());
                }
            }
            for file in &changed_names.removed_files {
                if crate::route_modules::may_be_a_module(file, None) {
                    modules.insert(file.clone());
                }
            }
            let file_set_changed =
                !changed_names.added_files.is_empty() || !changed_names.removed_files.is_empty();
            let sites = crate::route_modules::route_module_sites(
                store,
                &resolver,
                &modules,
                || {
                    surfaces.forwarding_closure(
                        store,
                        project_root,
                        scope.options.max_file_size,
                        &changed_names.js_surfaces,
                        &modules,
                        file_set_changed,
                    )
                },
                &reindexed,
            )?;
            for found in sites {
                if !dependent_fallbacks.contains(&found.file_path) {
                    merge_dependent_site(&mut dependent_sites, &mut dependent_fallbacks, found);
                }
            }
            // W7-31 — the two-hop half of lane B: a module the surface lane
            // re-resolves whole may import a value's class through the
            // barrel that moved, so the references written through a
            // binding into it go back too. Its nodes stay, and so do the
            // bindings into it.
            for file in &surfaces.files {
                changed_names.note_bindings(store, file)?;
            }
        }
        // W7-31 — lane B: a reference written through an import binding
        // (`logger.info()` after `import { logger } from './logging'`) reads
        // the bound module's text: the value's initializer or annotation and
        // its file's import of that class (#1292, #2644), or an object
        // literal's binding (#1932). No edge into the module and no touched
        // name reaches it, so when the module is re-extracted or removed,
        // or the surface lane re-resolves it whole, the references through
        // each binding into it go back by site with the binding.
        let mut bindings: BTreeMap<String, BTreeSet<ReferenceSite>> = BTreeMap::new();
        for importers in changed_names.bindings.values() {
            for (importer, sites) in importers {
                bindings
                    .entry(importer.clone())
                    .or_default()
                    .extend(sites.iter().cloned());
            }
        }
        binding_lane_sites(
            project_root,
            store,
            scope,
            &resolver,
            &bindings,
            &reindexed,
            stored_refs_marker.is_some(),
            &mut dependent_sites,
            &mut dependent_fallbacks,
        )?;
        let (mut scope_sites, fallback_files) = refresh_dependent_refs(
            project_root,
            store,
            scope,
            &resolver,
            &dependent_sites,
            &dependent_fallbacks,
            &receiver_dependents,
            &reindexed,
            stored_refs_marker.is_some(),
            &cross_file,
        )?;
        scope_sites.extend(cross_file_applied.inserted_sites);
        let mut scope_files = reindexed.clone();
        scope_files.extend(fallback_files);
        #[cfg(test)]
        if tests::FAIL_BEFORE_RESOLUTION.with(std::cell::Cell::get) {
            bail!("injected sync failure before resolution");
        }
        // The class-head rows were current when the sync began (no
        // bootstrap): a class head of a file it did not change is read from
        // its row, not its file (W5-08).
        if !class_heads_bootstrap {
            resolver.trust_recorded_class_heads(
                reindexed
                    .iter()
                    .chain(&changed_names.removed_files)
                    .cloned()
                    .collect(),
            );
        }
        // So were the bridge-facts rows: a file the sync did not change is
        // read from its row (W7-21).
        if !rn_bootstrap {
            resolver.trust_recorded_bridge_rows(
                reindexed
                    .iter()
                    .chain(&changed_names.removed_files)
                    .cloned()
                    .collect(),
            );
        }
        // A qualified reference (`util::double/1`) is retried by its whole
        // name too, however many share its tail (W5-05).
        let mut retry_names = changed_names.any.clone();
        retry_names.extend(changed_names.qualified.iter().cloned());
        // A sync whose touched names are all names of framework nodes a
        // re-extraction left out and the passes put back as they were has
        // nothing to wake: the store's nodes are what they were, so every
        // reference parked under one of those names sees the candidates it
        // saw. It skips the one pass over the parked references a retry by
        // name reads (W7-19a0).
        if changed_names.only_restored_nodes_touched(store)? {
            retry_names.clear();
        }
        #[cfg(test)]
        tests::note_retry_names(&retry_names);
        resolver.resolve_incremental_and_persist(
            store,
            &scope_files,
            &scope_sites,
            &retry_names,
        )?;
        // Last: the surface and namespace rows, then the markers the first
        // mutation withdrew.
        surfaces.write_rows(store)?;
        changed_names.binding_rows.write_rows(store)?;
        namespaces.write_rows(store)?;
        java.write_rows(store)?;
        kotlin.write_rows(store)?;
        scala.write_rows(store)?;
        csharp.write_rows(store)?;
        members.write_rows(store)?;
        ruby.write_rows(store)?;
        cfml.write_rows(store)?;
        solidity.write_rows(store)?;
        shopify.write_row(store, project_root)?;
        swift.write_rows(store)?;
        objc.write_rows(store)?;
        go.write_rows(store)?;
        php.write_rows(store)?;
        class_heads.write_rows(store)?;
        terraform.write_rows(store)?;
        rn_bridges.write_rows(store)?;
        control.write_rows(store)?;
        crate::control_inputs::write_frameworks(store, &frameworks)?;
        dart.write_rows(store)?;
        dart_libraries.write_rows(store)?;
        lua.write_rows(store)?;
        crate::lua_modules::write_loaders(store, &scope.options.lua_loaders)?;
        crate::module_surfaces::finish_bootstrap(store)?;
        crate::cpp_dependents::write_marker(store)?;
        crate::java_heads::write_marker(store)?;
        crate::kotlin_surfaces::write_marker(store)?;
        crate::scala_surfaces::write_marker(store)?;
        crate::csharp_surfaces::write_marker(store)?;
        crate::member_surfaces::write_marker(store)?;
        crate::ruby_heads::write_marker(store)?;
        crate::cfml_heads::write_marker(store)?;
        crate::solidity_heads::write_marker(store)?;
        crate::shopify_themes::write_marker(store)?;
        crate::swift_heads::write_marker(store)?;
        crate::objc_heads::write_marker(store)?;
        crate::go_heads::write_marker(store)?;
        crate::rust_receivers::write_marker(store)?;
        crate::php_heads::write_marker(store)?;
        crate::class_heads::write_marker(store)?;
        crate::terraform_wiring::write_marker(store)?;
        crate::control_inputs::write_marker(store)?;
        crate::case_folded::write_marker(store)?;
        crate::lua_modules::write_marker(store)?;
        crate::r_bindings::write_marker(store)?;
        crate::play_routes::write_marker(store)?;
        crate::laravel_routes::write_marker(store)?;
        crate::drupal_refs::write_marker(store)?;
        crate::rails_routes::write_marker(store)?;
        crate::vapor_routes::write_marker(store)?;
        crate::rn_bridges::write_marker(store)?;
        crate::rust_crate_roots::write_marker(store)?;
        crate::dart_heads::write_marker(store)?;
        crate::dart_libraries::write_marker(store)?;
        crate::resolved_refs::restore_marker(
            store,
            &scope.options,
            &scope.framework,
            stored_refs_marker.as_deref(),
        )?;
    }
    // A sync with nothing to resolve spent its time on the files alone.
    let rest = lap.take();
    if block {
        timings.resolve += rest;
    } else {
        timings.files += rest;
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
        // The sweep writes base edges: the synthesized ones go stale first.
        codegraph_resolve::synthesis::withdraw_marker(store)?;
        synthesis_reuse.whole(store)?;
        let stats = sweep_orphaned_refs(project_root, store, &scope.options)?;
        outcome.pending_refs_processed = stats.total;
        outcome.pending_refs_resolved = stats.resolved;
        outcome.pending_refs_unresolved = stats.unresolved;
    }
    timings.resolve += lap.take();

    // A file the last stage run read beside the index (W7-17b) that reads
    // differently now, its text, its presence or its scan scope changed,
    // makes the stage's edges stale as a base-graph write does. Every sync
    // compares, a sync that changed nothing included.
    if codegraph_resolve::synthesis::marker_is_current(store)?
        && codegraph_resolve::synthesis::inputs_changed(store, project_root, &scope.options)?
    {
        codegraph_resolve::synthesis::withdraw_marker(store)?;
    }

    // The dispatch edges no reference names (W7-14), recomputed whole once
    // the base graph is final. Every write to the base graph above comes
    // after a withdrawal of the marker (`begin_mutation`, a lane bootstrap,
    // the sweep, a changed input read beside the index), so a sync
    // recomputes whenever it finds the marker away: after its own changes,
    // or a sync that stopped before writing it back, which is this sync's
    // last write.
    if !codegraph_resolve::synthesis::marker_is_current(store)? {
        #[cfg(test)]
        if tests::FAIL_BEFORE_SYNTHESIS.with(std::cell::Cell::get) {
            bail!("injected sync failure before synthesis");
        }
        // The steps whose inputs the sync left alone keep their output when
        // the sync knows every base row it moved; anything else reruns the
        // stage whole (W9-00 row 27).
        #[cfg(test)]
        if let Some((path, text)) =
            tests::DRIFT_BEFORE_SYNTHESIS.with(|drift| drift.borrow_mut().take())
        {
            match text {
                Some(text) => fs::write(path, text)?,
                None => fs::remove_file(path)?,
            }
        }
        let reusing = match synthesis_reuse.take(store)? {
            Some(changes) => codegraph_resolve::synthesis::run_stage_reusing(
                store,
                project_root,
                &scope.options,
                &changes,
            )?,
            None => None,
        };
        let (stats, stage) = match reusing {
            Some(done) => done,
            None => {
                codegraph_resolve::synthesis::run_stage_timed(store, project_root, &scope.options)?
            }
        };
        outcome.synthesis = Some(Box::new(stats));
        timings.stage = Some(stage);
        #[cfg(test)]
        if tests::FAIL_BEFORE_SYNTHESIS_MARKER.with(std::cell::Cell::get) {
            bail!("injected sync failure before the synthesis marker");
        }
        codegraph_resolve::synthesis::write_marker(store)?;
    }
    synthesis_reuse.end(store)?;
    timings.synthesis += lap.take();

    outcome.duration_ms = started.elapsed().as_millis();
    let mut changed_paths: Vec<String> = reindexed.into_iter().collect();
    changed_paths.sort();
    outcome.changed_records = Some(changed_records(store, &changed_paths)?);
    outcome.changed_paths = changed_paths;
    outcome.control_paths = Some(crate::control_inputs::recorded_paths(store)?);
    timings.finish += lap.take();
    Ok(outcome)
}

/// Each of `paths` with its file row in `store` (`None` once removed), in
/// order: what [`SyncOutcome::changed_records`] carries.
pub(crate) fn changed_records(
    store: &Store,
    paths: &[String],
) -> Result<Vec<(String, Option<FileRecord>)>> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), store.file_by_path(path)?)))
        .collect()
}

/// Whether `name` is an import binding's local name (`logger`, `$store`):
/// one identifier, never a module specifier (`./logging`, `@app/kit`,
/// `app.logmod`).
fn is_binding_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// The local names `sites` bind among `refs`: the identifier names of the
/// `imports` references there. `None` when a site holds no `imports`
/// reference, which no selection by name can stand in for.
fn binding_names_at(
    refs: &[codegraph_core::types::UnresolvedRef],
    sites: &BTreeSet<ReferenceSite>,
) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for site in sites {
        let mut found = false;
        for reference in refs.iter().filter(|reference| {
            reference.reference_kind == codegraph_core::types::EdgeKind::Imports
                && reference.from_node_id == site.from_node_id
                && reference.line == site.line
                && reference.col == site.col
        }) {
            found = true;
            if is_binding_name(&reference.reference_name) {
                names.insert(reference.reference_name.clone());
            }
        }
        if !found {
            return None;
        }
    }
    Some(names)
}

/// Lane B's sites (W7-31): for each importer with a binding into a module
/// the sync rewrote, the sites of the references written through the
/// binding's local name. They come from the importer's stored references
/// while the stored-references marker holds (W5-09e), and from its
/// extraction otherwise, as `refresh_dependent_refs` reads a dependent's. A
/// binding site that holds no `imports` reference sends the importer whole.
/// An importer the sync re-extracted, or one already going whole, is
/// skipped.
#[allow(clippy::too_many_arguments)]
fn binding_lane_sites(
    project_root: &Path,
    store: &Store,
    scope: &ProjectScope,
    resolver: &ReferenceResolver,
    bindings: &BTreeMap<String, BTreeSet<ReferenceSite>>,
    reindexed: &HashSet<String>,
    stored_refs: bool,
    dependent_sites: &mut BTreeMap<String, BTreeSet<ReferenceSite>>,
    dependent_fallbacks: &mut BTreeSet<String>,
) -> Result<()> {
    for (importer, sites) in bindings {
        if reindexed.contains(importer)
            || dependent_fallbacks.contains(importer)
            || !project_root.join(importer).exists()
        {
            continue;
        }
        let through = if stored_refs {
            binding_names_at(&store.stored_refs_at_sites(sites)?, sites)
                .map(|names| store.stored_refs_through_names(importer, &names))
                .transpose()?
        } else {
            let refs = extract_dependent_refs(project_root, importer, scope, resolver)?;
            binding_names_at(&refs, sites).map(|names| {
                refs.iter()
                    .filter(|reference| {
                        reference.reference_kind != codegraph_core::types::EdgeKind::Imports
                            && names.iter().any(|name| {
                                reference
                                    .reference_name
                                    .strip_prefix(name.as_str())
                                    .is_some_and(|rest| rest.starts_with('.'))
                            })
                    })
                    .map(|reference| ReferenceSite {
                        from_node_id: reference.from_node_id.clone(),
                        line: reference.line,
                        col: reference.col,
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            })
        };
        let Some(through) = through else {
            #[cfg(test)]
            tests::note_binding_lane(importer, None);
            dependent_sites.remove(importer);
            dependent_fallbacks.insert(importer.clone());
            continue;
        };
        if through.is_empty() {
            continue;
        }
        #[cfg(test)]
        tests::note_binding_lane(importer, Some(through.len()));
        for site in through {
            merge_dependent_site(
                dependent_sites,
                dependent_fallbacks,
                FileReferenceSite {
                    file_path: importer.clone(),
                    site: Some(site),
                },
            );
        }
    }
    Ok(())
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
/// F's own dependents (an unbounded closure on hub symbols). So F's nodes stay
/// and only its references are read again: the stored ones while the marker
/// holds (W5-09e), otherwise an in-memory extraction merging base and framework
/// refs. Every captured site is refreshed as one indivisible sibling group;
/// unrelated edges and unresolved row ids stay untouched. A NULL historical edge
/// coordinate or a captured site with no fresh ref upgrades that file to the
/// correctness-first whole-file fallback, which still preserves nodes,
/// `contains` edges, and incoming edges. The returned site lane plus
/// fallback-file lane are resolved against the final graph.
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
    stored_refs: bool,
    cross_file: &codegraph_resolve::CrossFilePlan,
) -> Result<(BTreeSet<ReferenceSite>, HashSet<String>)> {
    let dependents = dependent_sites
        .keys()
        .chain(fallback_files)
        .chain(receiver_names.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut refreshed_sites = BTreeSet::new();
    let mut refreshed_files = HashSet::new();
    let mut replacements = Vec::new();
    let dependents = dependents
        .into_iter()
        .filter(|relative| !already_reindexed.contains(relative))
        .filter(|relative| project_root.join(relative).exists())
        .collect::<Vec<_>>();
    // An unchanged dependent's stored references are its last extraction's
    // while the marker holds (W5-09e), unless a framework's extraction of it
    // reads other files (`project.godot`); the rest are extracted.
    let from_store =
        |relative: &str| stored_refs && !resolver.framework_extraction_reads_other_files(relative);
    // Each dependent's extraction reads only its own file and the project
    // scope, so they run in parallel; the store is updated below in the
    // dependents' order, the first error in that order stopping it, as one
    // file after another did.
    let mut extracted: HashMap<String, Result<Vec<codegraph_core::types::UnresolvedRef>>> =
        dependents
            .iter()
            .filter(|relative| !from_store(relative))
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(|relative| {
                let refs = extract_dependent_refs(project_root, relative, scope, resolver).map(
                    |mut refs| {
                        // The nodes no single file's extraction makes keep
                        // their references (W7-09d).
                        refs.extend(cross_file.refs_of_file(relative).iter().cloned());
                        refs
                    },
                );
                (relative.clone(), refs)
            })
            .collect();
    for relative in dependents {
        // A dependent refreshed at its sites alone reads the stored references
        // there; one going whole, or read for its Rust receivers, all of them.
        let site_only =
            !fallback_files.contains(&relative) && !receiver_names.contains_key(&relative);
        let (refs, scoped) = match extracted.remove(&relative) {
            Some(refs) => (refs?, false),
            None => match dependent_sites.get(&relative).filter(|_| site_only) {
                Some(sites) => (store.stored_refs_at_sites(sites)?, true),
                None => (store.stored_refs_of_file(&relative)?, false),
            },
        };
        let sites = dependent_sites.get(&relative);
        let needs_fallback = fallback_files.contains(&relative)
            || sites.is_some_and(|sites| {
                sites.iter().any(|site| {
                    !refs
                        .iter()
                        .any(|reference| reference_matches_site(reference, site))
                })
            });
        #[cfg(test)]
        tests::note_dependent_refresh(&relative, from_store(&relative), needs_fallback);
        if needs_fallback {
            // A read scoped to the sites holds only theirs; the whole file
            // goes back with every reference it has.
            let refs = if scoped {
                store.stored_refs_of_file(&relative)?
            } else {
                refs
            };
            replacements.push(ResolutionReplacement::File {
                file_path: relative.clone(),
                refs,
            });
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
        replacements.push(ResolutionReplacement::Sites {
            sites: site_list,
            refs: selected,
        });
        refreshed_sites.extend(site_set);
    }
    // Every dependent's new state in one transaction: a commit per file cost
    // more than the writes themselves (W5-09a). Each dependent's references
    // were read before any of them changed, and no replacement touches
    // another file's rows.
    store.replace_resolution_states(&replacements)?;
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
    options: &ExtractOptions,
) -> Result<codegraph_resolve::ResolutionStats> {
    let mut resolver = ReferenceResolver::new(project_root.to_string_lossy())
        .with_max_file_size(options.max_file_size)
        .with_lua_loaders(Arc::clone(&options.lua_loaders));
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
    // Cross-file framework finalization before resolution, as a full index
    // runs it; a cross-file node that goes puts the references whose edges
    // reached it back among the pending ones the batch resolves.
    resolver.run_post_extract(store)?;
    resolver.run_cross_file_nodes(store)?;
    let result = resolver.resolve_and_persist_batched(store, ORPHAN_SWEEP_BATCH_ROWS)?;
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
    force_reextract: bool,
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
    if !force_reextract
        && let Some(file) = &stored
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
    if !force_reextract
        && stored
            .as_ref()
            .is_some_and(|file| file.content_hash == content_hash)
    {
        outcome.files_skipped_unchanged += 1;
        return Ok(false);
    }
    let language_before = stored.as_ref().map(|file| file.language);

    changed_names.begin_mutation(store)?;
    let mut importers = BTreeSet::new();
    for dependent in store.reference_sites_dependent_on_file(relative)? {
        importers.insert(dependent.file_path.clone());
        merge_dependent_site(dependent_sites, dependent_fallbacks, dependent);
    }
    // W7-31 — lane B's bindings into this module, before its nodes go. A
    // framework change re-extracts every file, and with it every importer.
    if !force_reextract
        && stored
            .as_ref()
            .is_some_and(|file| crate::binding_facts::is_lane_language(relative, file.language))
    {
        changed_names.note_bindings(store, relative)?;
    }
    // #2450 — an importer whose statement of this module holds a binding
    // the import did not resolve (`W` of `import { Widget as W } from
    // './widgets/Widget'`, parked or matched by its name) may bind to what
    // the edit gives the module. Read here: re-extraction drops the old
    // nodes, and the edges into them with them.
    if !added {
        changed_names
            .binding_importers
            .extend(store.importers_with_unbound_bindings(relative)?);
    }
    let language_after = detect_language_of(relative, &source, &scope.options.extensions);
    // Which files a control file's readers start from (W5-06).
    if language_before != Some(language_after) {
        changed_names.membership.push((
            relative.to_string(),
            language_before,
            Some(language_after),
        ));
    }
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
    // Whether a tracked file has its file node, before and after: crossing
    // `max_file_size` changes that without adding or removing the file. The
    // Rust module rule reads it (W7-19h), and so do the import retries, for
    // which such a file is one an import may now take or lose (W7-27).
    let file_node = (!added).then(|| codegraph_core::node_id::file_node_id(relative));
    let had_file_node = match &file_node {
        Some(id) => store.node_by_id(id)?.is_some(),
        None => false,
    };
    reextract_into_store(
        store,
        relative,
        scope,
        changed_names,
        (&metadata, &source, content_hash),
    )?;
    if let Some(id) = &file_node
        && store.node_by_id(id)?.is_some() != had_file_node
    {
        changed_names
            .file_nodes_changed
            .insert(relative.to_string());
    }
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
    changed_names
        .rn_bridges
        .note_importers(relative, &importers);
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
    changed_names
        .membership
        .push((relative.to_string(), Some(stored.language), None));
    let mut importers = BTreeSet::new();
    for dependent in store.reference_sites_dependent_on_file(relative)? {
        importers.insert(dependent.file_path.clone());
        merge_dependent_site(dependent_sites, dependent_fallbacks, dependent);
    }
    // W7-31 — lane B's bindings into the removed module, which fire
    // whatever its facts were, and its binding-facts row (OD-B72).
    if crate::binding_facts::is_lane_language(relative, stored.language) {
        changed_names.note_bindings(store, relative)?;
        changed_names.binding_rows.note(store, relative, None)?;
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
    changed_names.note_qualified(&old_nodes);
    changed_names.play.note(relative, &old_nodes);
    changed_names.laravel.note(relative, &old_nodes);
    changed_names.drupal.note(relative, &old_nodes);
    changed_names.rails.note(relative, &old_nodes);
    changed_names.vapor.note(relative, &old_nodes);
    changed_names
        .rn_bridges
        .note(store, relative, &old_nodes, None)?;
    changed_names
        .rn_bridges
        .note_importers(relative, &importers);
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
    changed_names.solidity.note(store, relative, None)?;
    changed_names
        .swift
        .note(store, relative, &old_nodes, None)?;
    changed_names.objc.note(store, relative, None)?;
    changed_names.go.note(store, relative, None)?;
    changed_names.php.note(store, relative, &old_nodes, None)?;
    changed_names.class_heads.note(store, relative, None)?;
    changed_names.terraform.note(store, relative, None)?;
    changed_names.lua.note(store, relative, None)?;
    changed_names.dart.note(store, relative, None)?;
    changed_names.dart_libraries.note(store, relative, None)?;
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
    changed_names.note_any(names);
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
    // `pairReindexedNodes`). A Dart import node is the other exception
    // (W4-DART-02): no resolution reads one, since a Dart `import`, `export`
    // or `part` links the file its URI names and no name binds an import
    // node, while its URI, when written as a bare file name, names every file
    // of that name (an `import 'internals.dart';` moving one line down
    // re-resolved riverpod's 112 imports of its `internals.dart` files).
    // Other languages read declaration text no node records (a C function's
    // `static`), so their moved nodes still touch.
    let old_nodes = store.nodes_by_file_path(relative)?;
    let mut touched = ChangedNames::default();
    touched.note_node_diff(&old_nodes, &result.nodes);
    // The stored nodes stay for the restore check when the comparison
    // touched a name: a framework's nodes are among them, and no
    // re-extraction makes those (W7-19a0).
    changed_names.note_restorable(relative, &old_nodes, &touched);
    changed_names.absorb_node_diff(touched);
    // What a Play action's choice read of this file (W7-19c), a Laravel
    // controller's and a Drupal handler's or hook's (W7-19d), a Rails route
    // target's (W7-19e) and a Vapor route handler's (W7-19g).
    changed_names.play.note(relative, &old_nodes);
    changed_names.laravel.note(relative, &old_nodes);
    changed_names.drupal.note(relative, &old_nodes);
    changed_names.rails.note(relative, &old_nodes);
    changed_names.vapor.note(relative, &old_nodes);
    // What the bridges read of this file's text and methods (W7-21).
    changed_names
        .rn_bridges
        .note(store, relative, &old_nodes, Some((source, &result.nodes)))?;
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
    changed_names.solidity.note(
        store,
        relative,
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
    changed_names.class_heads.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.terraform.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.lua.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names.dart.note(
        store,
        relative,
        Some((file.language, source, &result.nodes)),
    )?;
    changed_names
        .dart_libraries
        .note(store, relative, Some((file.language, source)))?;
    // An R call of a base function's name skips a data binding of it unless
    // the binding's line holds a function (upstream #2239), and that line is
    // read from source, which no node records: a changed R file touches the
    // names it binds that way, before the change and after it (W4-R-01).
    changed_names.note_all(
        codegraph_resolve::r_scope::r_base_bindings(&old_nodes)
            .chain(codegraph_resolve::r_scope::r_base_bindings(&result.nodes))
            .collect::<Vec<_>>(),
    );
    // No name in another file resolves into a minified script (upstream
    // #2204), and that verdict is read from the script's text: a script that
    // was or is generated can flip it with every node where it was, so all of
    // its names are touched and the refs naming them resolve afresh.
    let stored = store.file_by_path(relative)?;
    // W7-31 (OD-B72): lane B fires for an edited module only when its facts
    // moved (its import statements or module-level value declarations), or
    // when it had no row.
    if crate::binding_facts::is_lane_language(relative, file.language)
        || stored
            .as_ref()
            .is_some_and(|stored| crate::binding_facts::is_lane_language(relative, stored.language))
    {
        let row = crate::binding_facts::row_of(
            relative,
            file.language,
            source,
            &result.nodes,
            &result.edges,
        );
        if !changed_names.binding_rows.note(store, relative, row)?
            && let Some(quiet) = changed_names.bindings.remove(relative)
        {
            changed_names
                .quiet_bindings
                .insert(relative.to_string(), quiet);
        }
    }
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
    /// The files that came, went or changed language, with their language
    /// before and after: which files a control file's readers start from
    /// (W5-06).
    membership: Vec<crate::control_inputs::MembershipChange>,
    /// The qualified names (`util::double/1`, `app.CommonMapper::userColumns`)
    /// of every node whose name a changed candidate touched, on both sides of
    /// the change, and of a removed file's nodes. A reference written
    /// qualified waits under that whole name; its tail retry skips a tail
    /// more than the ceiling share, the whole-name lookup does not (W5-05).
    qualified: BTreeSet<String>,
    /// The files whose import of an edited module holds a binding the import
    /// did not resolve, read before the module's old nodes went (#2450):
    /// the edit may give the module what the binding imports under another
    /// name, which no name lookup finds.
    binding_importers: BTreeSet<String>,
    /// The import-binding sites other files of lane B's languages hold into
    /// each module this sync re-extracted or removed, or the surface lane
    /// re-resolves whole, by module and then importer, read while the
    /// module's nodes are there (W7-31): the references written through such
    /// a binding go back with it. A re-extracted module whose facts did not
    /// move is dropped again (OD-B72).
    bindings: BTreeMap<String, BTreeMap<String, BTreeSet<ReferenceSite>>>,
    /// The bindings of the re-extracted modules whose facts did not move,
    /// set aside: they fire after all when the surface lane walked, since
    /// what such a module's imports resolve to can move through a barrel
    /// they name, and the surface lane re-resolves no re-extracted file
    /// (OD-B72).
    quiet_bindings: BTreeMap<String, BTreeMap<String, BTreeSet<ReferenceSite>>>,
    /// The binding-facts rows of the files this sync rewrote (OD-B72).
    binding_rows: crate::binding_facts::RowChanges,
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
    /// The tracked files whose file node came or went as the sync
    /// re-extracted them: a file over `max_file_size` is recorded without
    /// nodes, so crossing the limit adds or removes its file node while the
    /// file stays tracked. The Rust module rule reads them (W7-19h), and the
    /// import retries take them as they take an added file (W7-27).
    file_nodes_changed: BTreeSet<String>,
    /// Whether this sync already withdrew the surface version marker, as it
    /// does before its first mutation and restores once everything persisted.
    surface_dirty: bool,
    /// The route nodes a navigation router may bind a call to, read before
    /// this sync's first mutation (W7-23b's N1, upstream #2425's
    /// `routesBefore`): the routes after the change are compared with them.
    /// `None` while the sync changed nothing.
    routes_before: Option<Vec<Node>>,
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
    /// What the changed files held that a bare Solidity call in other files
    /// reads: their types and `is` clauses (W4-SOL-01).
    solidity: crate::solidity_heads::SolidityChanges,
    swift: crate::swift_heads::SwiftChanges,
    /// What the changed files held that an Objective-C message in other
    /// files reads: class heads and property types (W4-SW-04).
    objc: crate::objc_heads::ObjcChanges,
    /// What the changed files held that a Go call in other files reads:
    /// type declarations' embeddings and field types (W4-GO-01).
    go: crate::go_heads::GoChanges,
    /// What the changed files held that a Play action's choice reads: class
    /// names and the names and languages of methods and functions (W7-19c).
    play: crate::play_routes::PlayChanges,
    /// What the changed files held that a Laravel controller's choice reads:
    /// class names and method names (W7-19d).
    laravel: crate::laravel_routes::LaravelChanges,
    /// What the changed files held that a Drupal handler's or hook's choice
    /// reads: class and method names, and hook functions (W7-19d).
    drupal: crate::drupal_refs::DrupalChanges,
    /// What the changed files held that a Rails route target's choice reads:
    /// Ruby class names and the names of methods and functions (W7-19e).
    rails: crate::rails_routes::RailsChanges,
    /// What the changed files held that a Vapor route handler's choice
    /// reads: the names of Swift methods and functions (W7-19g).
    vapor: crate::vapor_routes::VaporChanges,
    /// What the changed files held that the React Native, Expo and
    /// Swift-Objective-C bridges read: their bridge facts, and their
    /// Objective-C and JVM methods (W7-21).
    rn_bridges: crate::rn_bridges::BridgeChanges,
    /// The Rust type, trait and alias names that came, went or changed: a
    /// CamelCase path receiver of such a name is the project's or an outside
    /// type by it (W4-RS-03).
    rust_receiver_types: BTreeSet<String>,
    /// What the changed files held that a PHP call in other files reads:
    /// class, trait, interface and enum heads (W4-PHP-01).
    php: crate::php_heads::PhpChanges,
    /// What the changed files' type declarations' heads were and are, for a
    /// walk from a class name (W5-07).
    class_heads: crate::class_heads::ClassHeadChanges,
    /// What the changed Terraform files declared that a module bridge reads
    /// (W7-24 cost, OD-B73).
    terraform: crate::terraform_wiring::WiringChanges,
    /// What the changed files held that a Lua call in other files reads:
    /// their locals and module surface (W4-LUA-01).
    lua: crate::lua_modules::LuaChanges,
    /// What the changed files held that a bare Dart call in other files
    /// reads: their types' heads (W4-DART-01).
    dart: crate::dart_heads::DartChanges,
    /// What the changed files held that a Dart name or import in other files
    /// reads: their directives (W4-DART-02).
    dart_libraries: crate::dart_libraries::DartLibraryChanges,
    /// Whether every name this sync touched may be a name of stored nodes its
    /// framework passes put back as they were (W7-19a0): a name is touched
    /// either by a changed file's node comparison or through
    /// [`Self::note_all`] and [`Self::note_any`], which stop the check.
    restored: RestoreCheck,
}

/// What a sync keeps to tell, once its framework passes have run, whether the
/// names it touched were touched only by framework nodes a re-extraction left
/// out and the passes put back as they were
/// ([`ChangedNames::only_restored_nodes_touched`]).
#[derive(Debug, Default)]
struct RestoreCheck {
    /// The stored nodes of each changed file whose node comparison touched a
    /// name, from before the change, by file.
    files: BTreeMap<String, Vec<Node>>,
    /// How many nodes `files` holds, at most [`RESTORE_CHECK_NODE_LIMIT`].
    nodes: usize,
    /// Set once the sync touched a name some other way, would keep more nodes
    /// than the limit, or had a framework pass change a node outside `files`:
    /// its retry by name then runs as it always does.
    abandoned: bool,
}

impl RestoreCheck {
    fn abandon(&mut self) {
        self.abandoned = true;
        self.files.clear();
        self.nodes = 0;
    }
}

/// How many stored nodes a sync keeps for its restore check. The check saves
/// one pass over the parked references, which a sync of a few files notices; a
/// larger sync retries by name rather than hold its files' old nodes.
const RESTORE_CHECK_NODE_LIMIT: usize = 10_000;

/// Whether `a` and `b` are the same node but for when each was written.
fn same_but_timestamp(a: &Node, b: &Node) -> bool {
    let mut a = a.clone();
    a.updated_at = b.updated_at;
    a == *b
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
            // Read before anything changes, whatever the frameworks are:
            // they are detected only once the changed files are written.
            self.routes_before = Some(crate::navigation::route_snapshot(store)?);
            store.delete_project_metadata(codegraph_resolve::module_surface::VERSION_KEY)?;
            crate::cpp_dependents::withdraw_marker(store)?;
            crate::java_heads::withdraw_marker(store)?;
            crate::kotlin_surfaces::withdraw_marker(store)?;
            crate::scala_surfaces::withdraw_marker(store)?;
            crate::csharp_surfaces::withdraw_marker(store)?;
            crate::member_surfaces::withdraw_marker(store)?;
            crate::ruby_heads::withdraw_marker(store)?;
            crate::cfml_heads::withdraw_marker(store)?;
            crate::solidity_heads::withdraw_marker(store)?;
            crate::shopify_themes::withdraw_marker(store)?;
            crate::swift_heads::withdraw_marker(store)?;
            crate::objc_heads::withdraw_marker(store)?;
            crate::go_heads::withdraw_marker(store)?;
            crate::rust_receivers::withdraw_marker(store)?;
            crate::php_heads::withdraw_marker(store)?;
            crate::class_heads::withdraw_marker(store)?;
            crate::terraform_wiring::withdraw_marker(store)?;
            crate::control_inputs::withdraw_marker(store)?;
            crate::case_folded::withdraw_marker(store)?;
            crate::lua_modules::withdraw_marker(store)?;
            crate::r_bindings::withdraw_marker(store)?;
            crate::play_routes::withdraw_marker(store)?;
            crate::laravel_routes::withdraw_marker(store)?;
            crate::drupal_refs::withdraw_marker(store)?;
            crate::rails_routes::withdraw_marker(store)?;
            crate::vapor_routes::withdraw_marker(store)?;
            crate::rn_bridges::withdraw_marker(store)?;
            crate::rust_crate_roots::withdraw_marker(store)?;
            crate::dart_heads::withdraw_marker(store)?;
            crate::dart_libraries::withdraw_marker(store)?;
            crate::resolved_refs::withdraw_marker(store)?;
            codegraph_resolve::synthesis::withdraw_marker(store)?;
            self.surface_dirty = true;
        }
        Ok(())
    }

    /// Record the import bindings other files of lane B's languages hold
    /// into `relative` (W7-31), while its nodes are still there. A binding
    /// edge with no coordinates already sent its importer whole (the
    /// dependents of `relative` hold the same edge).
    fn note_bindings(&mut self, store: &Store, relative: &str) -> Result<()> {
        for (found, language) in store.import_binding_sites_into_file(relative)? {
            if !crate::binding_facts::is_lane_language(&found.file_path, language) {
                continue;
            }
            if let Some(site) = found.site {
                self.bindings
                    .entry(relative.to_string())
                    .or_default()
                    .entry(found.file_path)
                    .or_default()
                    .insert(site);
            }
        }
        Ok(())
    }

    /// What an import of an added file, or of a tracked one whose file node
    /// came or went (W7-27), can be written as, and the namespaces and
    /// modules the changed files declare (upstream #2392).
    fn import_retry_keys(&self) -> (Vec<String>, Vec<String>) {
        let mut path_keys = BTreeSet::new();
        for file in self.added_files.iter().chain(&self.file_nodes_changed) {
            path_keys.extend(codegraph_store::import_path_keys(file));
        }
        let mut names: Vec<String> = self.module_names.iter().cloned().collect();
        names.sort_unstable();
        (path_keys.into_iter().collect(), names)
    }

    /// The qualified names of `nodes` a qualified reference can be written
    /// as: those with a `::`.
    fn note_qualified<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>) {
        self.qualified.extend(
            nodes
                .into_iter()
                .filter(|node| node.qualified_name.contains("::"))
                .map(|node| node.qualified_name.clone()),
        );
    }

    /// Every node of a file left the index: all its names are touched.
    fn note_removed(&mut self, names: HashSet<String>) {
        self.note_all(names);
    }

    /// Touch every one of `names`, as a ref outside their file may now see
    /// them differently.
    fn note_all(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            self.restored.abandon();
            self.candidates.insert(name.clone());
            self.any.insert(name);
        }
    }

    /// Stop the restore check: this sync retries by name as it always does.
    fn abandon_restore_check(&mut self) {
        self.restored.abandon();
    }

    /// Touch `names` for the name lane alone: what a name reaches moved
    /// though no node of it changed (a Python re-export, a module global's
    /// readers).
    fn note_any(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            self.restored.abandon();
            self.any.insert(name);
        }
    }

    /// Touch the names whose nodes differ between a file's stored nodes
    /// (`before`) and the ones its re-extraction made (`after`), the
    /// comparison `reextract_into_store` describes: an id that came or went
    /// touches its name, unless only a PHP node or a Dart import moved and
    /// the order of that name's nodes held, and [`Self::note_candidates`]
    /// adds the names whose candidates changed.
    fn note_node_diff(&mut self, before: &[Node], after: &[Node]) {
        let old_ids: HashSet<(&str, &str)> = before
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        let new_ids: HashSet<(&str, &str)> = after
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        let order_before = positioned_candidate_keys(before);
        let order_after = positioned_candidate_keys(after);
        let touched_when_moved: HashSet<&str> = before
            .iter()
            .chain(after)
            .filter(|node| {
                node.language != Language::Php
                    && !(node.language == Language::Dart && node.kind == NodeKind::Import)
            })
            .map(|node| node.name.as_str())
            .collect();
        for (_, name) in old_ids.symmetric_difference(&new_ids) {
            if touched_when_moved.contains(*name)
                || order_before.get(*name) != order_after.get(*name)
            {
                self.any.insert((*name).to_string());
            }
        }
        self.note_candidates(before, after);
    }

    /// Whether [`Self::note_node_diff`] touched nothing.
    fn touches_nothing(&self) -> bool {
        self.any.is_empty()
            && self.candidates.is_empty()
            && self.qualified.is_empty()
            && self.rust_receiver_types.is_empty()
    }

    /// What one file's [`Self::note_node_diff`] touched, added to this sync's.
    fn absorb_node_diff(&mut self, touched: ChangedNames) {
        self.any.extend(touched.any);
        self.candidates.extend(touched.candidates);
        self.qualified.extend(touched.qualified);
        self.rust_receiver_types.extend(touched.rust_receiver_types);
    }

    /// Keep `file`'s stored nodes (`before`) for the restore check when its
    /// node comparison (`touched`) touched a name.
    fn note_restorable(&mut self, file: &str, before: &[Node], touched: &ChangedNames) {
        if self.restored.abandoned || touched.touches_nothing() {
            return;
        }
        if self.restored.files.contains_key(file)
            || self.restored.nodes + before.len() > RESTORE_CHECK_NODE_LIMIT
        {
            self.restored.abandon();
            return;
        }
        self.restored.nodes += before.len();
        self.restored
            .files
            .insert(file.to_string(), before.to_vec());
    }

    /// The framework extraction of the changed files made nodes in `files`:
    /// one in a file the check does not keep is a node no comparison saw.
    fn note_framework_node_files(&mut self, files: &BTreeSet<String>) {
        if files
            .iter()
            .any(|file| !self.restored.files.contains_key(file))
        {
            self.restored.abandon();
        }
    }

    /// `updates` are what the post-extract passes are about to write: one
    /// that changes a stored node outside the files the check keeps changes a
    /// node no comparison saw.
    fn note_post_extract_updates(&mut self, store: &Store, updates: &[Node]) -> Result<()> {
        if self.restored.abandoned {
            return Ok(());
        }
        let outside: Vec<&Node> = updates
            .iter()
            .filter(|node| !self.restored.files.contains_key(&node.file_path))
            .collect();
        if outside.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = outside.iter().map(|node| node.id.clone()).collect();
        let stored = store.nodes_by_ids(&ids)?;
        if outside.iter().any(|update| {
            stored
                .get(&update.id)
                .is_none_or(|node| !same_but_timestamp(node, update))
        }) {
            self.restored.abandon();
        }
        Ok(())
    }

    /// Whether every name this sync touched is one its framework passes made
    /// moot (W7-19a0): only node comparisons touched names, no pass changed a
    /// node outside the files they compared, and each of those files' nodes,
    /// read back now, touch nothing against its stored ones. The store's
    /// nodes are then what they were before the sync, so a retry by those
    /// names would wake references whose candidates are what they were.
    fn only_restored_nodes_touched(&self, store: &Store) -> Result<bool> {
        if self.restored.abandoned {
            return Ok(false);
        }
        for (file, before) in &self.restored.files {
            let after = store.nodes_by_file_path(file)?;
            let mut touched = ChangedNames::default();
            touched.note_node_diff(before, &after);
            if !touched.touches_nothing() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Record each name whose nodes differ between `before` and `after` as a ref
    /// outside the file sees them, comparing the two sides as multisets of
    /// [`candidate_key`]s.
    fn note_candidates(&mut self, before: &[Node], after: &[Node]) {
        // No resolution reads a Dart import node (W4-DART-02; see
        // `reextract_into_store`): an edited directive touches nothing by its
        // URI.
        let candidate =
            |node: &&Node| !(node.language == Language::Dart && node.kind == NodeKind::Import);
        let mut keys: BTreeMap<&str, (Vec<String>, Vec<String>)> = BTreeMap::new();
        for node in before.iter().filter(candidate) {
            keys.entry(&node.name)
                .or_default()
                .0
                .push(candidate_key(node));
        }
        for node in after.iter().filter(candidate) {
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
        // A candidate's key holds its qualified name, which its id does not:
        // a mapper namespace or an Erlang `-module` renamed in place moves
        // the name a qualified reference waits under, though no id changes.
        let moved: Vec<&Node> = before
            .iter()
            .chain(after)
            .filter(|node| candidate(node) && touched.contains(node.name.as_str()))
            .collect();
        self.note_qualified(moved);
        for node in before.iter().chain(after) {
            if crate::rust_receivers::is_receiver_type(node) && touched.contains(node.name.as_str())
            {
                self.rust_receiver_types.insert(node.name.clone());
            }
        }
        // A bare Erlang call is written with its arity (`do_get/3`, upstream
        // #1610) and waits for it under that name, which no node carries: a
        // function that came, went or changed touches it, so a call parked
        // until the module it imports from, or a header, declares the
        // function is retried however many share its name (W4-ERL-01).
        for node in before.iter().chain(after) {
            if node.language == Language::Erlang
                && node.kind == NodeKind::Function
                && touched.contains(node.name.as_str())
                && let Some(written) = node.qualified_name.rsplit("::").next()
                && written.len() > node.name.len()
                && written.starts_with(node.name.as_str())
                && written[node.name.len()..].starts_with('/')
            {
                self.any.insert(written.to_string());
                self.candidates.insert(written.to_string());
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
pub(crate) fn candidate_key(node: &Node) -> String {
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
    // The references resolution kept go with the ones it did not (W5-09e).
    store
        .connection()
        .execute("DELETE FROM resolved_refs WHERE file_path = ?", [relative])?;
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
        /// Fail the next sync that recomputes synthesis once every other
        /// write is done, before the stage.
        pub(crate) static FAIL_BEFORE_SYNTHESIS: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next sync that recomputes synthesis once the stage has
        /// replaced the edges, before the marker goes back.
        pub(crate) static FAIL_BEFORE_SYNTHESIS_MARKER: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// The change log's cap for the syncs of this thread, in place of
        /// the default (W9-00 row 27).
        pub(crate) static CHANGE_LOG_CAP: std::cell::Cell<Option<usize>> =
            const { std::cell::Cell::new(None) };
        /// A file the next sync rewrites, or removes when the text is
        /// `None`, just before its synthesis stage reads, as an editor or a
        /// checkout racing the sync would (H2).
        pub(crate) static DRIFT_BEFORE_SYNTHESIS:
            std::cell::RefCell<Option<(std::path::PathBuf, Option<String>)>> =
            const { std::cell::RefCell::new(None) };
        /// Fail the next Lua row rebuild once the old rows are purged and
        /// one is written back.
        pub(crate) static FAIL_AFTER_LUA_ROW_PURGE: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next sync that extracts every Lua file again for new
        /// loader settings, once it has (W9-02).
        pub(crate) static FAIL_AFTER_LUA_LOADER_REEXTRACT: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next Dart row rebuild once the old rows are purged and
        /// one is written back.
        pub(crate) static FAIL_AFTER_DART_ROW_PURGE: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next Dart library row rebuild once the old rows are
        /// purged and one is written back.
        pub(crate) static FAIL_AFTER_DART_LIBRARY_ROW_PURGE: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next sync once the theme lane recorded what it found
        /// moved, before the per-path pass.
        pub(crate) static FAIL_AFTER_SHOPIFY_REFRESH: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// Fail the next sync once its changed files' base references are
        /// written, before their framework references.
        pub(crate) static FAIL_BEFORE_FRAMEWORK_REFS: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// How the syncs on this thread refreshed each dependent file.
        static DEPENDENT_REFRESHES: std::cell::RefCell<Vec<DependentRefresh>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// What lane B (W7-31) did with each importer on this thread: the
        /// number of sites it selected, or `None` when the importer went
        /// whole.
        static BINDING_LANE: std::cell::RefCell<Vec<(String, Option<usize>)>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// The names each sync on this thread retried parked references by,
        /// sorted.
        static RETRY_NAMES: std::cell::RefCell<Vec<Vec<String>>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// The steps the navigation lanes took on this thread (W7-23b):
        /// `routes` when the routes after a change were read, `calls` when
        /// the calls a changed route may answer were looked up,
        /// `destinations` when a dependent's navigation calls were.
        static NAVIGATION_STEPS: std::cell::RefCell<Vec<&'static str>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    pub(crate) fn note_navigation(step: &'static str) {
        NAVIGATION_STEPS.with(|steps| steps.borrow_mut().push(step));
    }

    /// The navigation lanes' steps since the last call, in order.
    fn take_navigation_steps() -> Vec<&'static str> {
        NAVIGATION_STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut()))
    }

    pub(crate) fn note_binding_lane(importer: &str, sites: Option<usize>) {
        BINDING_LANE.with(|lane| lane.borrow_mut().push((importer.to_string(), sites)));
    }

    /// Lane B's decisions since the last call, sorted by importer.
    fn take_binding_lane() -> Vec<(String, Option<usize>)> {
        let mut lane = BINDING_LANE.with(|lane| std::mem::take(&mut *lane.borrow_mut()));
        lane.sort();
        lane
    }

    pub(crate) fn note_retry_names(names: &HashSet<String>) {
        let mut names: Vec<String> = names.iter().cloned().collect();
        names.sort();
        RETRY_NAMES.with(|retries| retries.borrow_mut().push(names));
    }

    /// The names each sync since the last call retried by, in sync order.
    fn take_retry_names() -> Vec<Vec<String>> {
        RETRY_NAMES.with(|retries| std::mem::take(&mut *retries.borrow_mut()))
    }

    /// One dependent file a sync refreshed: whether its references came
    /// from the store (W5-09e) and whether it went whole.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct DependentRefresh {
        pub(crate) file: String,
        pub(crate) stored: bool,
        pub(crate) whole: bool,
    }

    pub(crate) fn note_dependent_refresh(file: &str, stored: bool, whole: bool) {
        DEPENDENT_REFRESHES.with(|refreshes| {
            refreshes.borrow_mut().push(DependentRefresh {
                file: file.to_string(),
                stored,
                whole,
            });
        });
    }

    /// The dependent refreshes since the last call, sorted by file.
    fn take_dependent_refreshes() -> Vec<DependentRefresh> {
        let mut refreshes =
            DEPENDENT_REFRESHES.with(|refreshes| std::mem::take(&mut *refreshes.borrow_mut()));
        refreshes.sort_by(|a, b| a.file.cmp(&b.file));
        refreshes
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

    const SYNTH_SCENE: (&str, &str) = (
        "src/scene.ts",
        "type Callback = () => void;\nexport class Scene {\n  private callbacks = new Set<Callback>();\n  onUpdate(cb: Callback) {\n    this.callbacks.add(cb);\n  }\n  triggerUpdate() {\n    for (const cb of this.callbacks) cb();\n  }\n}\n",
    );
    const SYNTH_RENDER: (&str, &str) = (
        "src/app.ts",
        "import { Scene } from './scene';\nexport class App {\n  scene = new Scene();\n  constructor() {\n    this.scene.onUpdate(this.triggerRender);\n  }\n  triggerRender() {\n    return 1;\n  }\n  triggerPaint() {\n    return 2;\n  }\n}\n",
    );
    const SYNTH_PAINT: (&str, &str) = (
        "src/app.ts",
        "import { Scene } from './scene';\nexport class App {\n  scene = new Scene();\n  constructor() {\n    this.scene.onUpdate(this.triggerPaint);\n  }\n  triggerRender() {\n    return 1;\n  }\n  triggerPaint() {\n    return 2;\n  }\n}\n",
    );

    fn synthesis_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(codegraph_resolve::synthesis::VERSION_KEY)
            .unwrap()
    }

    fn synthesized_edges(db: &Path) -> Vec<String> {
        edge_projection(db)
            .into_iter()
            .filter(|edge| edge.contains("synthesizedBy"))
            .collect()
    }

    /// Insert a synthesized edge no pass would make, between two nodes named
    /// `source` and `target`: the stage's replacement must remove it.
    fn plant_stale_synthesized_edge(db: &Path, source: &str, target: &str) {
        let mut store = Store::open(db).unwrap();
        let id = |name: &str| store.nodes_by_name(name).unwrap().pop().unwrap().id;
        let edge = codegraph_core::types::Edge {
            id: None,
            source: id(source),
            target: id(target),
            kind: codegraph_core::types::EdgeKind::Calls,
            metadata: Some(
                serde_json::json!({"synthesizedBy": "callback", "registeredAt": "stale"}),
            ),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        };
        store.insert_edges(&[edge]).unwrap();
        assert!(
            synthesized_edges(db)
                .iter()
                .any(|edge| edge.contains("stale"))
        );
    }

    // ----- W9-00 row 27: the steps a sync keeps -----------------------------

    const BUS_SAVE: (&str, &str) = (
        "src/save.ts",
        "export function save(bus: any) {\n  bus.emit('saved');\n}\n",
    );
    const BUS_SAVE_AGAIN: (&str, &str) = (
        "src/save.ts",
        "export function save(bus: any) {\n  bus.emit('saved');\n  bus.emit('again');\n}\n",
    );
    const BUS_ON: (&str, &str) = (
        "src/on.ts",
        "export function onSaved() {\n  return 1;\n}\nexport function wire(bus: any) {\n  bus.on('saved', onSaved);\n}\n",
    );
    const OTHER: (&str, &str) = ("src/other.ts", "export const other = 1;\n");
    const OTHER_EDITED: (&str, &str) = ("src/other.ts", "export const other = 2;\n");

    /// What the last sync's stage did.
    fn stage_of(outcome: &SyncOutcome) -> codegraph_resolve::synthesis::StageStats {
        *outcome
            .synthesis
            .as_deref()
            .expect("the sync runs the synthesis stage")
    }

    fn ran_step(stats: &codegraph_resolve::synthesis::StageStats, step: &str) -> bool {
        codegraph_resolve::synthesis::step_names()
            .enumerate()
            .any(|(at, name)| name == step && stats.rerun & (1 << at) != 0)
    }

    /// An index of the callback channel and the emitter files, and an
    /// unrelated file the tests edit.
    fn reuse_fixture(name: &str) -> (TestDir, PathBuf) {
        let dir = TestDir::new(name);
        let db = default_db_path(dir.path()).unwrap();
        write_files(
            dir.path(),
            &[SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER],
        );
        sync_changed_paths(
            dir.path(),
            &db,
            [SYNTH_SCENE.0, SYNTH_RENDER.0, BUS_SAVE.0, BUS_ON.0, OTHER.0],
        )
        .unwrap();
        (dir, db)
    }

    /// An edit no step reads keeps every step's output (W9-00 row 27); the
    /// edges are a fresh index's.
    #[test]
    fn a_sync_keeps_the_steps_its_edit_does_not_reach() {
        let (dir, db) = reuse_fixture("row27-keep");
        write_files(dir.path(), &[OTHER_EDITED]);
        let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
        let stats = stage_of(&outcome);
        assert_eq!(stats.rerun, 0, "{stats:?}");
        assert!(stats.reused > 0, "{stats:?}");
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "row27-keep-fresh",
                &[SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED]
            )
        );
    }

    /// The whole-project sync's stage timings.
    fn timed_sync(dir: &Path) -> (SyncOutcome, codegraph_resolve::synthesis::StageTimings) {
        let (outcome, timings) = sync_project_once_with_progress_timed(dir, |_, _| {}).unwrap();
        let stage = timings
            .and_then(|timings| timings.stage)
            .expect("the sync runs the synthesis stage");
        (outcome, stage)
    }

    /// The emitter channel with its handler in a file of its own, which no
    /// pass's per-file check passes, so an edit there keeps every step.
    const RESTORE_ON: (&str, &str) = (
        "src/on.ts",
        "import { onSaved } from './handler';\nexport function wire(bus: any) {\n  bus.on('saved', onSaved);\n}\n",
    );
    const RESTORE_HANDLER: (&str, &str) = (
        "src/handler.ts",
        "export function onSaved() {\n  return 1;\n}\n",
    );

    /// An edit that keeps every step but re-extracts a file the stored edges
    /// touch (W9-00 row 27): the handler's file, so the emitter edge into it
    /// goes with the cascade. The sync writes it back from the change log,
    /// reads no step's output row, and leaves a fresh index's edges.
    #[test]
    fn a_sync_that_keeps_every_step_writes_back_the_cascade() {
        let dir = TestDir::new("row27-restore");
        let db = default_db_path(dir.path()).unwrap();
        let files = [BUS_SAVE, RESTORE_ON, RESTORE_HANDLER, OTHER];
        write_files(dir.path(), &files);
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        assert!(
            synthesized_edges(&db)
                .iter()
                .any(|edge| edge.contains("onSaved")),
            "{:?}",
            synthesized_edges(&db)
        );
        let handler = format!("{}// note\n", RESTORE_HANDLER.1);
        write_files(dir.path(), &[(RESTORE_HANDLER.0, handler.as_str())]);
        let (outcome, stage) = timed_sync(dir.path());
        let stats = stage_of(&outcome);
        assert_eq!(stats.rerun, 0, "{stats:?}");
        assert!(stage.restore.is_some(), "{stage:?}");
        assert_eq!(stats.removed, 0, "{stats:?}");
        assert!(stats.inserted > 0, "the cascade dropped an edge: {stats:?}");
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "row27-restore-fresh",
                &[
                    BUS_SAVE,
                    RESTORE_ON,
                    (RESTORE_HANDLER.0, handler.as_str()),
                    OTHER
                ]
            )
        );
    }

    /// The `:edges` row of `step` in `db`'s records.
    fn edges_row_key(step: &str) -> String {
        format!(
            "{}{step}{}",
            codegraph_resolve::synthesis::PASS_KEY_PREFIX,
            codegraph_resolve::synthesis::EDGES_KEY_SUFFIX
        )
    }

    /// The record rows a fresh index of `files` writes.
    fn fresh_records(name: &str, files: &[(&str, &str)]) -> Vec<(String, String)> {
        let dir = TestDir::new(name);
        write_files(dir.path(), files);
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        record_rows(&db)
    }

    /// Plant `row` as `step`'s `:edges` row, its head naming the row's length;
    /// with `hash` its SHA-256 too, so only the row's text can tell.
    fn plant_output_row(db: &Path, step: &str, row: &str, hash: bool) {
        let store = Store::open(db).unwrap();
        let key = format!("{}{step}", codegraph_resolve::synthesis::PASS_KEY_PREFIX);
        let mut head: serde_json::Value =
            serde_json::from_str(&store.get_project_metadata(&key).unwrap().unwrap()).unwrap();
        head["edgesBytes"] = serde_json::json!(row.len());
        if hash {
            head["edgesHash"] = serde_json::json!(codegraph_core::node_id::hash_content(row));
        }
        store.set_project_metadata(&key, &head.to_string()).unwrap();
        store
            .set_project_metadata(&edges_row_key(step), row)
            .unwrap();
    }

    /// An output row that is not the one its head names, though of the
    /// length named (W9-00 row 27's fix round): the field step's edge turned
    /// round. A sync that keeps every step reads no output row and keeps the
    /// step; the sync whose merge reads the row finds its hash is not the
    /// head's and runs the stage whole, so the turned edge never reaches the
    /// graph. The edges and the records are then a fresh index's.
    #[test]
    fn an_output_row_is_merged_only_when_it_is_the_one_its_head_names() {
        let (dir, db) = reuse_fixture("row27-mismatch");
        // Without the change, the emitter edit reruns the emitter step and
        // merges the field step's kept output.
        write_files(dir.path(), &[BUS_SAVE_AGAIN]);
        let stats = stage_of(&sync_changed_paths(dir.path(), &db, [BUS_SAVE_AGAIN.0]).unwrap());
        assert!(ran_step(&stats, "emitterEdges"), "{stats:?}");
        assert!(!ran_step(&stats, "fieldEdges"), "{stats:?}");
        let row = Store::open(&db)
            .unwrap()
            .get_project_metadata(&edges_row_key("fieldEdges"))
            .unwrap()
            .unwrap();
        let json = codegraph_resolve::synthesis::decode_output_row(&row).unwrap();
        let mut edges: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(edges.len(), 1, "{json}");
        let source = edges[0]["source"].clone();
        edges[0]["source"] = edges[0]["target"].clone();
        edges[0]["target"] = source;
        let turned = codegraph_resolve::synthesis::encode_output_row(
            &serde_json::to_string(&edges).unwrap(),
        );
        assert_ne!(turned, row);
        plant_output_row(&db, "fieldEdges", &turned, false);
        write_files(dir.path(), &[OTHER_EDITED]);
        let (outcome, stage) = timed_sync(dir.path());
        let stats = stage_of(&outcome);
        assert_eq!(stats.rerun, 0, "{stats:?}");
        assert!(stats.reused > 0, "{stats:?}");
        assert!(stage.restore.is_some(), "{stage:?}");
        write_files(dir.path(), &[BUS_SAVE]);
        let outcome = sync_changed_paths(dir.path(), &db, [BUS_SAVE.0]).unwrap();
        assert_eq!(stage_of(&outcome).reused, 0, "{:?}", outcome.synthesis);
        let files = [SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("row27-mismatch-fresh", &files)
        );
        assert_eq!(
            record_rows(&db),
            fresh_records("row27-mismatch-records", &files)
        );
    }

    /// An output row its head names, length and hash, that does not decode
    /// (W9-00 row 27's fix round): text that is not base64, base64 of bytes
    /// that do not inflate (a reserved deflate block type), and a stream that
    /// inflates to text that is not JSON. The sync whose merge reads it runs
    /// the stage whole, and the edges and the records are a fresh index's.
    #[test]
    fn an_output_row_that_does_not_decode_runs_the_stage_whole() {
        let cases = [
            ("not-base64", "not base64!".to_string()),
            ("not-deflate", "BwAA".to_string()),
            (
                "not-json",
                codegraph_resolve::synthesis::encode_output_row("not json"),
            ),
        ];
        for (label, row) in cases {
            let (dir, db) = reuse_fixture(&format!("row27-undecodable-{label}"));
            plant_output_row(&db, "fieldEdges", &row, true);
            write_files(dir.path(), &[BUS_SAVE_AGAIN]);
            let outcome = sync_changed_paths(dir.path(), &db, [BUS_SAVE_AGAIN.0]).unwrap();
            assert_eq!(
                stage_of(&outcome).reused,
                0,
                "{label}: {:?}",
                outcome.synthesis
            );
            let files = [SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE_AGAIN, BUS_ON, OTHER];
            assert_eq!(
                edge_projection(&db),
                fresh_edges(&format!("row27-undecodable-{label}-fresh"), &files),
                "{label}"
            );
            assert_eq!(
                record_rows(&db),
                fresh_records(&format!("row27-undecodable-{label}-records"), &files),
                "{label}"
            );
        }
    }

    /// A step that runs again with another output rewrites both its rows
    /// (W9-00 row 27's fix round): the registration moves, the field step
    /// runs again while the others keep theirs, and its footprint row and
    /// its `:edges` row are a fresh index's, byte for byte, as are the edges.
    #[test]
    fn a_step_that_runs_again_rewrites_both_its_rows() {
        let (dir, db) = reuse_fixture("row27-rewrite");
        let before = Store::open(&db)
            .unwrap()
            .get_project_metadata(&edges_row_key("fieldEdges"))
            .unwrap();
        write_files(dir.path(), &[SYNTH_PAINT]);
        let stats = stage_of(&sync_changed_paths(dir.path(), &db, [SYNTH_PAINT.0]).unwrap());
        assert!(ran_step(&stats, "fieldEdges"), "{stats:?}");
        assert!(stats.reused > 0, "{stats:?}");
        let after = Store::open(&db)
            .unwrap()
            .get_project_metadata(&edges_row_key("fieldEdges"))
            .unwrap();
        assert_ne!(after, before, "the field step's output moved");
        let files = [SYNTH_SCENE, SYNTH_PAINT, BUS_SAVE, BUS_ON, OTHER];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("row27-rewrite-fresh", &files)
        );
        assert_eq!(
            record_rows(&db),
            fresh_records("row27-rewrite-records", &files)
        );
    }

    /// An `:edges` row deleted by hand (W9-00 row 27's fix round): the next
    /// sync, though its edit keeps every step and reads no output row, finds
    /// the row missing and runs the stage whole; the edges and the records
    /// are then a fresh index's.
    #[test]
    fn a_missing_output_row_runs_the_stage_whole() {
        let (dir, db) = reuse_fixture("row27-missing");
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(&edges_row_key("fieldEdges"))
            .unwrap();
        write_files(dir.path(), &[OTHER_EDITED]);
        let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
        assert_eq!(stage_of(&outcome).reused, 0, "{:?}", outcome.synthesis);
        let files = [SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("row27-missing-fresh", &files)
        );
        assert_eq!(
            record_rows(&db),
            fresh_records("row27-missing-records", &files)
        );
    }

    /// Records of another layout or version (W9-00 row 27's fix round) run
    /// the stage whole and are written again as a fresh index writes them:
    /// the single-row layout the pre-fixup 27c build left (version 1, the
    /// output inside the record, no `:edges` row), and heads of this layout
    /// stamped with 27c's version 1, both under the stage record this build
    /// wrote, so only the records' layout or version tells them apart. The
    /// second is what bumping the version answers.
    #[test]
    fn records_of_another_layout_or_version_run_the_stage_whole() {
        for label in ["27c-layout", "27c-version"] {
            let (dir, db) = reuse_fixture(&format!("row27-layout-{label}"));
            {
                let store = Store::open(&db).unwrap();
                for (key, value) in record_rows(&db) {
                    let Some(step) =
                        key.strip_prefix(codegraph_resolve::synthesis::PASS_KEY_PREFIX)
                    else {
                        continue;
                    };
                    if step.ends_with(codegraph_resolve::synthesis::EDGES_KEY_SUFFIX) {
                        continue;
                    }
                    let mut head: serde_json::Value = serde_json::from_str(&value).unwrap();
                    head["version"] = serde_json::json!(1);
                    if label == "27c-layout" {
                        let fields = head.as_object_mut().unwrap();
                        fields.remove("edgesBytes");
                        fields.remove("edgesHash");
                        fields.insert("edges".to_string(), serde_json::json!([]));
                        store.delete_project_metadata(&edges_row_key(step)).unwrap();
                    }
                    store.set_project_metadata(&key, &head.to_string()).unwrap();
                }
            }
            write_files(dir.path(), &[OTHER_EDITED]);
            let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
            assert_eq!(
                stage_of(&outcome).reused,
                0,
                "{label}: {:?}",
                outcome.synthesis
            );
            let files = [SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED];
            assert_eq!(
                edge_projection(&db),
                fresh_edges(&format!("row27-layout-{label}-fresh"), &files),
                "{label}"
            );
            assert_eq!(
                record_rows(&db),
                fresh_records(&format!("row27-layout-{label}-records"), &files),
                "{label}"
            );
        }
    }

    /// Each fallback runs the stage whole (W9-00 row 27): the marker away
    /// when the sync began, the stage record lost, written by another build,
    /// over other options, a lane bootstrap, the orphan sweep, and a change
    /// log past its cap. Each leaves a fresh index's edges.
    #[test]
    fn each_fallback_runs_the_stage_whole() {
        type Plant = fn(&Store);
        let fallbacks: [(&str, Plant); 6] = [
            ("marker", |store| {
                store
                    .delete_project_metadata(codegraph_resolve::synthesis::VERSION_KEY)
                    .unwrap();
            }),
            ("record", |store| {
                store
                    .delete_project_metadata(codegraph_resolve::synthesis::STAGE_KEY)
                    .unwrap();
            }),
            ("build", |store| {
                rewrite_stage_record(store, "build", serde_json::json!("0.0.0+another"));
            }),
            ("options", |store| {
                rewrite_stage_record(store, "options", serde_json::json!("another"));
            }),
            ("bootstrap", |store| {
                store
                    .delete_project_metadata(codegraph_resolve::module_surface::VERSION_KEY)
                    .unwrap();
            }),
            ("sweep", |store| {
                store.set_resolution_incomplete().unwrap();
            }),
        ];
        for (label, plant) in fallbacks {
            let (dir, db) = reuse_fixture(&format!("row27-whole-{label}"));
            plant(&Store::open(&db).unwrap());
            write_files(dir.path(), &[OTHER_EDITED]);
            let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
            assert_eq!(
                stage_of(&outcome).reused,
                0,
                "{label}: {:?}",
                outcome.synthesis
            );
            assert_eq!(
                edge_projection(&db),
                fresh_edges(
                    &format!("row27-whole-{label}-fresh"),
                    &[SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED]
                ),
                "{label}"
            );
        }
        let (dir, db) = reuse_fixture("row27-whole-overflow");
        write_files(dir.path(), &[OTHER_EDITED]);
        CHANGE_LOG_CAP.with(|cap| cap.set(Some(1)));
        let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]);
        CHANGE_LOG_CAP.with(|cap| cap.set(None));
        assert_eq!(stage_of(&outcome.unwrap()).reused, 0, "overflow");
    }

    /// One field of the stored `synthesis_passes` row set to `value`.
    fn rewrite_stage_record(store: &Store, field: &str, value: serde_json::Value) {
        let key = codegraph_resolve::synthesis::STAGE_KEY;
        let mut record: serde_json::Value =
            serde_json::from_str(&store.get_project_metadata(key).unwrap().unwrap()).unwrap();
        record[field] = value;
        store
            .set_project_metadata(key, &record.to_string())
            .unwrap();
    }

    /// A step whose record head no longer reads as this engine's runs the
    /// stage whole (W9-00 row 27's fix round: a record is never read in
    /// part); a step whose record says it read what the view cannot see
    /// into runs again alone.
    #[test]
    fn a_step_whose_record_does_not_read_runs_again() {
        let (dir, db) = reuse_fixture("row27-record");
        Store::open(&db)
            .unwrap()
            .set_project_metadata(
                &format!(
                    "{}emitterEdges",
                    codegraph_resolve::synthesis::PASS_KEY_PREFIX
                ),
                "{\"version\":0}",
            )
            .unwrap();
        write_files(dir.path(), &[OTHER_EDITED]);
        let stats = stage_of(&sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap());
        assert_eq!(stats.reused, 0, "{stats:?}");
        assert!(ran_step(&stats, "emitterEdges"), "{stats:?}");
        assert!(ran_step(&stats, "fieldEdges"), "{stats:?}");
        // A record that says its step read what the view cannot see into
        // (a probe on disk, a listing, a drifted file) is not kept either.
        {
            let store = Store::open(&db).unwrap();
            let key = format!(
                "{}fieldEdges",
                codegraph_resolve::synthesis::PASS_KEY_PREFIX
            );
            let mut record: serde_json::Value =
                serde_json::from_str(&store.get_project_metadata(&key).unwrap().unwrap()).unwrap();
            record["reusable"] = serde_json::json!(false);
            store
                .set_project_metadata(&key, &record.to_string())
                .unwrap();
        }
        write_files(dir.path(), &[OTHER]);
        let stats = stage_of(&sync_changed_paths(dir.path(), &db, [OTHER.0]).unwrap());
        assert!(ran_step(&stats, "fieldEdges"), "{stats:?}");
        assert!(!ran_step(&stats, "emitterEdges"), "{stats:?}");
        write_files(dir.path(), &[OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "row27-record-fresh",
                &[SYNTH_SCENE, SYNTH_RENDER, BUS_SAVE, BUS_ON, OTHER_EDITED]
            )
        );
    }

    /// H2: a sync's emitter step reads a file that drifts under it, and the
    /// file comes back as it was indexed, which no later change log shows.
    /// The step's record is not reused, so the next sync, which changes an
    /// unrelated file, runs it again and finds the handler.
    #[test]
    fn a_step_that_read_a_drifted_file_runs_again() {
        for (label, drift) in [("rewritten", Some("// drifted\n")), ("removed", None)] {
            a_step_that_read_a_drifted_file_runs_again_when(label, drift.map(str::to_string));
        }
    }

    fn a_step_that_read_a_drifted_file_runs_again_when(label: &str, drift: Option<String>) {
        let (dir, db) = reuse_fixture(&format!("row27-drift-{label}"));
        write_files(dir.path(), &[BUS_SAVE_AGAIN]);
        DRIFT_BEFORE_SYNTHESIS.with(|hook| {
            *hook.borrow_mut() = Some((dir.path().join(BUS_ON.0), drift));
        });
        let drifted = sync_changed_paths(dir.path(), &db, [BUS_SAVE_AGAIN.0]).unwrap();
        assert!(ran_step(&stage_of(&drifted), "emitterEdges"));
        assert!(
            !synthesized_edges(&db)
                .iter()
                .any(|edge| edge.contains("event-emitter")),
            "the drifted handler is read as absent"
        );
        write_files(dir.path(), &[BUS_ON]);
        write_files(dir.path(), &[OTHER_EDITED]);
        let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
        assert!(ran_step(&stage_of(&outcome), "emitterEdges"));
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                &format!("row27-drift-{label}-fresh"),
                &[
                    SYNTH_SCENE,
                    SYNTH_RENDER,
                    BUS_SAVE_AGAIN,
                    BUS_ON,
                    OTHER_EDITED
                ]
            ),
            "{label}"
        );
    }

    /// The `synthesis_pass` rows of `db`.
    fn record_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix("synthesis_pass")
            .unwrap()
    }

    /// H3: a build that knows no step records syncs a change, recomputing
    /// the stage whole and writing its own marker value, and leaves the
    /// records of the change before; or a build of other sources wrote the
    /// records. Either way the next sync runs the stage whole and the edges
    /// are a fresh index's.
    #[test]
    fn records_another_build_left_are_not_trusted() {
        for label in ["older-build", "other-build"] {
            let (dir, db) = reuse_fixture(&format!("row27-h3-{label}"));
            let before = record_rows(&db);
            write_files(dir.path(), &[SYNTH_PAINT]);
            sync_changed_paths(dir.path(), &db, [SYNTH_PAINT.0]).unwrap();
            {
                let store = Store::open(&db).unwrap();
                for (key, value) in &before {
                    store.set_project_metadata(key, value).unwrap();
                }
                if label == "older-build" {
                    store
                        .set_project_metadata(codegraph_resolve::synthesis::VERSION_KEY, "23")
                        .unwrap();
                } else {
                    rewrite_stage_record(
                        &store,
                        "build",
                        serde_json::json!("0.55.2+0000000000000000"),
                    );
                }
            }
            write_files(dir.path(), &[OTHER_EDITED]);
            let outcome = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
            assert_eq!(stage_of(&outcome).reused, 0, "{label}");
            assert_eq!(
                edge_projection(&db),
                fresh_edges(
                    &format!("row27-h3-{label}-fresh"),
                    &[SYNTH_SCENE, SYNTH_PAINT, BUS_SAVE, BUS_ON, OTHER_EDITED]
                ),
                "{label}"
            );
        }
    }

    /// The stage follows an edit (W7-14): the registration moves to another
    /// method, and the sync's synthesized edges are a fresh index's.
    #[test]
    fn a_sync_recomputes_the_synthesized_edges() {
        let dir = TestDir::new("synthesis-edit");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();
        let before = synthesized_edges(&db);
        assert!(
            before.len() == 1 && before[0].contains("App::triggerRender"),
            "{before:?}"
        );
        assert_eq!(
            synthesis_marker(&db).as_deref(),
            Some(codegraph_resolve::synthesis::VERSION)
        );

        write_files(dir.path(), &[SYNTH_PAINT]);
        sync_changed_paths(dir.path(), &db, [SYNTH_PAINT.0]).unwrap();
        let after = synthesized_edges(&db);
        assert!(
            after.len() == 1 && after[0].contains("App::triggerPaint"),
            "{after:?}"
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("synthesis-edit-fresh", &[SYNTH_SCENE, SYNTH_PAINT])
        );
    }

    /// A sync that stops before the stage, or after the replacement and
    /// before the marker, leaves the marker away (W7-14): the next sync, with
    /// nothing changed, recomputes and writes it back.
    #[test]
    fn a_sync_stopped_before_its_synthesis_marker_leaves_the_next_to_recompute() {
        for (label, hook) in [
            ("stage", &FAIL_BEFORE_SYNTHESIS),
            ("marker", &FAIL_BEFORE_SYNTHESIS_MARKER),
        ] {
            let dir = TestDir::new(&format!("synthesis-stop-{label}"));
            let db = default_db_path(dir.path()).unwrap();
            write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
            sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();

            write_files(dir.path(), &[SYNTH_PAINT]);
            hook.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [SYNTH_PAINT.0]);
            hook.with(|fail| fail.set(false));
            assert!(
                failed.is_err(),
                "{label}: the injected failure must stop the sync"
            );
            assert_eq!(
                synthesis_marker(&db),
                None,
                "{label}: the marker stays away"
            );

            sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
            assert_eq!(
                synthesis_marker(&db).as_deref(),
                Some(codegraph_resolve::synthesis::VERSION),
                "{label}"
            );
            assert_eq!(
                edge_projection(&db),
                fresh_edges(
                    &format!("synthesis-stop-{label}-fresh"),
                    &[SYNTH_SCENE, SYNTH_PAINT]
                ),
                "{label}"
            );
        }
    }

    /// A marker another version of the passes wrote is not current (W7-14c
    /// moved it to "2"): a sync with nothing changed replaces that
    /// version's synthesized edges and writes this version's marker.
    #[test]
    fn a_synthesis_marker_of_another_version_recomputes() {
        let dir = TestDir::new("synthesis-version");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();
        plant_stale_synthesized_edge(&db, "triggerPaint", "onUpdate");
        Store::open(&db)
            .unwrap()
            .set_project_metadata(codegraph_resolve::synthesis::VERSION_KEY, "1")
            .unwrap();

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            synthesis_marker(&db).as_deref(),
            Some(codegraph_resolve::synthesis::VERSION)
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("synthesis-version-fresh", &[SYNTH_SCENE, SYNTH_RENDER])
        );
    }

    /// A lane bootstrap re-resolves without `begin_mutation`, so it withdraws
    /// the synthesis marker before its first write (W7-14): a sync that
    /// stops after the lane markers went back leaves it away, and the next
    /// sync replaces a stale synthesized edge.
    #[test]
    fn a_lane_bootstrap_withdraws_the_synthesis_marker() {
        let dir = TestDir::new("synthesis-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();
        plant_stale_synthesized_edge(&db, "triggerPaint", "onUpdate");
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(codegraph_resolve::module_surface::VERSION_KEY)
            .unwrap();

        FAIL_BEFORE_SYNTHESIS.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_BEFORE_SYNTHESIS.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert!(
            surface_marker(&db).is_some(),
            "the bootstrap's lane marker went back"
        );
        assert_eq!(synthesis_marker(&db), None);

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges("synthesis-bootstrap-fresh", &[SYNTH_SCENE, SYNTH_RENDER])
        );
    }

    /// Table 9.1 row 2 (upstream a2f3c31): the orphan sweep resolves its
    /// parked references in batches of [`ORPHAN_SWEEP_BATCH_ROWS`], each next
    /// batch resolving while the one before it is written. Over more than one
    /// batch, with calls whose receiver's type reaches the method through an
    /// `extends` edge the wiring batch resolves, the healed index equals a
    /// fresh one.
    #[test]
    fn an_orphan_sweep_over_several_batches_converges() {
        let calls_per_function = 100;
        let functions = ORPHAN_SWEEP_BATCH_ROWS / calls_per_function + 2;
        let app: String = (0..functions)
            .map(|f| {
                let calls: String = (0..calls_per_function).map(|_| "  s.ping();\n").collect();
                format!("export function run{f}(): void {{\n  const s = new Sub();\n{calls}}}\n")
            })
            .collect();
        let app = format!("import {{ Sub }} from './sub';\n{app}");
        let files = [
            (
                "src/base.ts",
                "export class Base {\n  ping(): number {\n    return 1;\n  }\n}\n",
            ),
            (
                "src/sub.ts",
                "import { Base } from './base';\nexport class Sub extends Base {}\n",
            ),
            ("src/app.ts", app.as_str()),
        ];
        let dir = TestDir::new("sweep-batches");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &files);
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        // Park every resolved reference again, without the edges it made, as
        // a full index stopped before its resolution would leave them.
        {
            let store = Store::open(&db).unwrap();
            let parked = store
                .connection()
                .execute(
                    "INSERT INTO unresolved_refs (id, from_node_id, reference_name, reference_kind, \
                     line, col, candidates, file_path, language, reference_subkind) \
                     SELECT id, from_node_id, reference_name, reference_kind, line, col, \
                     candidates, file_path, language, reference_subkind \
                     FROM resolved_refs ORDER BY id",
                    [],
                )
                .unwrap();
            assert!(
                parked > ORPHAN_SWEEP_BATCH_ROWS,
                "the sweep needs more than one batch: {parked} parked"
            );
            store
                .connection()
                .execute_batch(
                    "DELETE FROM resolved_refs; \
                     DELETE FROM edges WHERE json_extract(metadata, '$.resolvedBy') IS NOT NULL;",
                )
                .unwrap();
            store.set_resolution_incomplete().unwrap();
        }
        assert_ne!(
            edge_projection(&db),
            fresh_edges("sweep-batches-parked", &files),
            "the parked index lacks the resolved edges"
        );
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(outcome.pending_refs_processed > ORPHAN_SWEEP_BATCH_ROWS);
        assert!(
            !Store::open(&db)
                .unwrap()
                .is_resolution_incomplete()
                .unwrap()
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("sweep-batches-fresh", &files)
        );
    }

    /// The orphan sweep resolves new base edges on a sync with nothing
    /// changed, so the stage recomputes after it (W7-14).
    #[test]
    fn the_orphan_sweep_recomputes_the_synthesized_edges() {
        let dir = TestDir::new("synthesis-sweep");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();
        plant_stale_synthesized_edge(&db, "triggerPaint", "onUpdate");
        Store::open(&db)
            .unwrap()
            .set_resolution_incomplete()
            .unwrap();
        assert_eq!(
            synthesis_marker(&db).as_deref(),
            Some(codegraph_resolve::synthesis::VERSION)
        );

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(
            !Store::open(&db)
                .unwrap()
                .is_resolution_incomplete()
                .unwrap()
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("synthesis-sweep-fresh", &[SYNTH_SCENE, SYNTH_RENDER])
        );
    }

    /// The orphan sweep withdraws the synthesis marker before its first
    /// write (W7-14): a sync that stops after sweeping leaves it away, and
    /// the next sync, with nothing to sweep, replaces a stale synthesized
    /// edge.
    #[test]
    fn an_orphan_sweep_withdraws_the_synthesis_marker() {
        let dir = TestDir::new("synthesis-sweep-stop");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0]).unwrap();
        plant_stale_synthesized_edge(&db, "triggerPaint", "onUpdate");
        Store::open(&db)
            .unwrap()
            .set_resolution_incomplete()
            .unwrap();

        FAIL_BEFORE_SYNTHESIS.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_BEFORE_SYNTHESIS.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert!(
            !Store::open(&db)
                .unwrap()
                .is_resolution_incomplete()
                .unwrap()
        );
        assert_eq!(synthesis_marker(&db), None);

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges("synthesis-sweep-stop-fresh", &[SYNTH_SCENE, SYNTH_RENDER])
        );
    }

    /// A file that changed on disk since it was indexed feeds no pass until
    /// its own sync (W7-14): its old nodes' lines would slice its new text.
    /// Its lines need not have moved for that: the stage cannot tell. A sync
    /// of another file keeps the field step whose inputs it left alone
    /// (W9-00 row 27), so the channel the step found while the scene read as
    /// indexed stays, beside the scene's indexed nodes, and nothing slices
    /// the drifted text; a step that runs again reads the scene as absent.
    #[test]
    fn a_drifted_file_feeds_no_synthesis_until_its_sync() {
        const SCENE_DRIFTED: (&str, &str) = (
            "src/scene.ts",
            "type Callback = () => void;\nexport class Scene {\n  private callbacks = new Set<Callback>();\n  onUpdate(cb: Callback) {\n    this.callbacks.add(cb);\n  }\n  triggerUpdate() {\n    for (const callback of this.callbacks) callback();\n  }\n}\n",
        );
        const OTHER: (&str, &str) = ("src/other.ts", "export const other = 1;\n");
        const OTHER_EDITED: (&str, &str) = ("src/other.ts", "export const other = 2;\n");
        let dir = TestDir::new("synthesis-drift");
        let db = default_db_path(dir.path()).unwrap();
        write_files(dir.path(), &[SYNTH_SCENE, SYNTH_RENDER, OTHER]);
        sync_changed_paths(dir.path(), &db, [SYNTH_SCENE.0, SYNTH_RENDER.0, OTHER.0]).unwrap();
        assert_eq!(synthesized_edges(&db).len(), 1);

        // The scene drifts; a sync of another file keeps the field step.
        write_files(dir.path(), &[SCENE_DRIFTED, OTHER_EDITED]);
        let kept = sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();
        assert!(
            !ran_step(&stage_of(&kept), "fieldEdges"),
            "{:?}",
            kept.synthesis
        );
        assert_eq!(synthesized_edges(&db).len(), 1);

        // A sync whose field step runs again (its record no longer reads)
        // reads the drifted scene as absent and hides the channel.
        Store::open(&db)
            .unwrap()
            .set_project_metadata(
                &format!(
                    "{}fieldEdges",
                    codegraph_resolve::synthesis::PASS_KEY_PREFIX
                ),
                "{}",
            )
            .unwrap();
        write_files(dir.path(), &[OTHER]);
        let rerun = sync_changed_paths(dir.path(), &db, [OTHER.0]).unwrap();
        assert!(
            ran_step(&stage_of(&rerun), "fieldEdges"),
            "{:?}",
            rerun.synthesis
        );
        assert_eq!(synthesized_edges(&db), Vec::<String>::new());
        write_files(dir.path(), &[OTHER_EDITED]);
        sync_changed_paths(dir.path(), &db, [OTHER_EDITED.0]).unwrap();

        // Its own sync brings the channel back, as a fresh index has it.
        sync_changed_paths(dir.path(), &db, [SCENE_DRIFTED.0]).unwrap();
        assert_eq!(synthesized_edges(&db).len(), 1);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "synthesis-drift-fresh",
                &[SCENE_DRIFTED, SYNTH_RENDER, OTHER_EDITED]
            )
        );
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

    /// W4-DART-03: Dart files have member surface rows too (a member read
    /// types its receiver by the fields of the classes around it, a chain
    /// link by their statements), so an index whose member marker is
    /// missing, or at a version before the rows held what is read now,
    /// re-resolves every Dart file once and writes their rows.
    #[test]
    fn an_index_without_the_member_marker_rebuilds_the_dart_rows() {
        const SHAPES: (&str, &str) = ("lib/shapes.dart", "class Box {\n  int get area => 1;\n}\n");
        const BASE: (&str, &str) = (
            "lib/base.dart",
            "import 'shapes.dart';\n\nclass Base {\n  Base(this.shape);\n  final Box shape;\n}\n",
        );
        const SUB: (&str, &str) = (
            "lib/sub.dart",
            "import 'base.dart';\n\nclass Sub extends Base {\n  Sub(super.shape);\n\n  int measure() => shape.area;\n}\n",
        );
        const SPARE: (&str, &str) = ("lib/spare.dart", "int spare() => 1;\n");
        const SPARE_EDITED: (&str, &str) = ("lib/spare.dart", "int spare() => 2;\n");
        let version = Some(crate::member_surfaces::VERSION.to_string());
        let dir = TestDir::new("member-surface-dart-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [("pubspec.yaml", "name: app\n"), SHAPES, BASE, SUB, SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(member_marker(&db), version);
        let rows = member_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "member_surface:lib/base.dart"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.contains("Sub::measure -[calls]-> lib/shapes.dart")),
            "{expected:#?}"
        );
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Box::area')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the read edge an older index lacked");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::member_types::SURFACE_ROW_KEY_PREFIX,
                )
                .unwrap();
            // The version before Dart rows kept their statements
            // (W4-DART-03b).
            store
                .set_project_metadata(crate::member_surfaces::VERSION_KEY, "3")
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["lib/spare.dart"]).unwrap();
        assert_eq!(member_marker(&db), version);
        assert_eq!(member_rows(&db), rows);
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

    fn class_heads_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::class_heads::VERSION_KEY)
            .unwrap()
    }

    fn class_head_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(crate::class_heads::ROW_KEY_PREFIX)
            .unwrap()
    }

    const PY_BASE: (&str, &str) = (
        "pkg/base.py",
        "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n",
    );
    const PY_OTHER: (&str, &str) = (
        "pkg/other.py",
        "class Other:\n    @classmethod\n    def create(cls):\n        return cls()\n",
    );
    const PY_USER_BASE: (&str, &str) = (
        "pkg/user.py",
        "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass User(Base):\n    pass\n",
    );
    const PY_USER_OTHER: (&str, &str) = (
        "pkg/user.py",
        "from pkg.base import Base\nfrom pkg.other import Other\n\n\nclass User(Other):\n    pass\n",
    );
    const PY_SERVICE: (&str, &str) = (
        "pkg/service.py",
        "from pkg.user import User\n\n\ndef make():\n    return User.create()\n",
    );
    const PY_SPARE: (&str, &str) = ("pkg/spare.py", "def idle():\n    return 0\n");
    const PY_SPARE_EDITED: (&str, &str) = ("pkg/spare.py", "def idle():\n    return 1\n");

    /// W5-07: a sync failing after its mutations leaves the class-head marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every file a walk from a class name reads and
    /// rebuilds the rows: `User.create()`, whose class now extends the other
    /// base, ends where a fresh index puts it.
    #[test]
    fn a_class_head_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_other = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("make -[calls]-> pkg/other.py:Other::create"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "class-heads-fail-unrelated"
            } else {
                "class-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [PY_BASE, PY_OTHER, PY_USER_BASE, PY_SERVICE, PY_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                class_heads_marker(&db),
                Some(crate::class_heads::VERSION.to_string())
            );
            assert!(!to_other(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[PY_USER_OTHER]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [PY_USER_OTHER.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                class_heads_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[PY_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [PY_SPARE.0]).unwrap();
                PY_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                PY_SPARE
            };
            assert_eq!(
                class_heads_marker(&db),
                Some(crate::class_heads::VERSION.to_string())
            );
            let fresh = [PY_BASE, PY_OTHER, PY_USER_OTHER, PY_SERVICE, spare];
            let name = if unrelated {
                "class-heads-fresh-unrelated"
            } else {
                "class-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_other(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W5-07: an index whose class-head marker and rows alone are gone (the
    /// other markers written back) has every file a walk reads re-resolved
    /// and every row rebuilt by its next sync, even one with nothing
    /// changed: the class-name call an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_class_head_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::class_heads::VERSION.to_string());
        let dir = TestDir::new("class-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [PY_BASE, PY_OTHER, PY_USER_BASE, PY_SERVICE, PY_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(class_heads_marker(&db), version);
        let rows = class_head_rows(&db);
        assert!(
            rows.iter().any(|(key, _)| key == "class_heads:pkg/user.py"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Base::create')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(crate::class_heads::ROW_KEY_PREFIX)
                .unwrap();
            store
                .delete_project_metadata(crate::class_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            php_marker(&db),
            Some(crate::php_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(class_heads_marker(&db), version);
        assert_eq!(class_head_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn metadata(db: &Path, key: &str) -> Option<String> {
        Store::open(db).unwrap().get_project_metadata(key).unwrap()
    }

    const TS_CONFIG_PLAIN: (&str, &str) = ("tsconfig.json", "{\"compilerOptions\":{}}\n");
    const TS_CONFIG_PATHS: (&str, &str) = (
        "tsconfig.json",
        "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/lib/*\"]}}}\n",
    );
    const TS_MATH: (&str, &str) = (
        "src/lib/math.ts",
        "export function double(value: number): number {\n  return value * 2;\n}\n",
    );
    const TS_MAIN: (&str, &str) = (
        "src/main.ts",
        "import { double } from '@lib/math';\n\nexport function run(): number {\n  return double(2);\n}\n",
    );
    const TS_SPARE: (&str, &str) = ("src/spare.ts", "export const spare = 0;\n");
    const TS_SPARE_EDITED: (&str, &str) = ("src/spare.ts", "export const spare = 1;\n");

    /// Drops the import edge of `double`, as an older sync could have lost it.
    const DROP_DOUBLE_IMPORT: &str = "DELETE FROM edges WHERE kind = 'imports' AND target = \
         (SELECT id FROM nodes WHERE name = 'double' AND kind = 'function')";

    fn imports_double(db: &Path) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains("src/main.ts -[imports]-> src/lib/math.ts:double"))
    }

    /// W5-06: a sync failing after a control file's change left the control
    /// marker away, so the next sync, with nothing changed or with an
    /// unrelated change, re-resolves every JS-family file and rewrites the
    /// rows: the alias import ends where a fresh index puts it.
    #[test]
    fn a_control_input_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "control-inputs-fail-unrelated"
            } else {
                "control-inputs-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [TS_CONFIG_PLAIN, TS_MATH, TS_MAIN, TS_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                metadata(&db, crate::control_inputs::VERSION_KEY),
                Some(crate::control_inputs::VERSION.to_string())
            );
            assert!(!imports_double(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[TS_CONFIG_PATHS]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [TS_CONFIG_PATHS.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                metadata(&db, crate::control_inputs::VERSION_KEY),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[TS_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [TS_SPARE.0]).unwrap();
                TS_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                TS_SPARE
            };
            assert_eq!(
                metadata(&db, crate::control_inputs::VERSION_KEY),
                Some(crate::control_inputs::VERSION.to_string())
            );
            let fresh = [TS_CONFIG_PATHS, TS_MATH, TS_MAIN, spare];
            let name = if unrelated {
                "control-inputs-fresh-unrelated"
            } else {
                "control-inputs-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(imports_double(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W5-06: an index whose control marker alone is gone re-resolves every
    /// file of the three kinds once, but re-extracts none; one whose
    /// frameworks marker alone is gone re-extracts every file.
    #[test]
    fn a_missing_marker_reresolves_and_a_missing_frameworks_marker_reextracts() {
        let dir = TestDir::new("control-inputs-markers");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [TS_CONFIG_PATHS, TS_MATH, TS_MAIN, TS_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store.connection().execute(DROP_DOUBLE_IMPORT, []).unwrap();
            assert_eq!(dropped, 1, "the import edge an older sync lost");
            store
                .delete_project_metadata(crate::control_inputs::VERSION_KEY)
                .unwrap();
        }
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "no file is re-extracted");
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(
            metadata(&db, crate::control_inputs::VERSION_KEY),
            Some(crate::control_inputs::VERSION.to_string())
        );

        Store::open(&db)
            .unwrap()
            .delete_project_metadata(crate::control_inputs::FRAMEWORKS_VERSION_KEY)
            .unwrap();
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            outcome.files_reindexed, 3,
            "every source file is re-extracted"
        );
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_VERSION_KEY),
            Some(crate::control_inputs::FRAMEWORKS_VERSION.to_string())
        );
    }

    /// W5-06: a control row that cannot be read counts as changed: its kind
    /// re-resolves whole and the row is rewritten.
    #[test]
    fn a_corrupt_control_row_counts_as_changed() {
        let dir = TestDir::new("control-inputs-corrupt");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [TS_CONFIG_PATHS, TS_MATH, TS_MAIN];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        let row_key = crate::control_inputs::ControlKind::Js.row_key();
        let row = metadata(&db, &row_key).expect("the JS row");
        {
            let store = Store::open(&db).unwrap();
            store.connection().execute(DROP_DOUBLE_IMPORT, []).unwrap();
            store.set_project_metadata(&row_key, "not json").unwrap();
        }
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
    }

    /// W7-19b: an index from before the `python` control kind has no row for
    /// it. The first sync counts the kind as moved, so it detects the
    /// frameworks (here as before: nothing is extracted again), writes the
    /// row and leaves the graph as it was; the next sync does not count it.
    #[test]
    fn a_missing_python_control_row_detects_the_frameworks_once() {
        let dir = TestDir::new("control-inputs-python-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            ("manage.py", "import sys\n"),
            (
                "shop/urls.py",
                "from django.urls import path\nfrom . import views\n\nurlpatterns = [\n    path(\"products/\", views.product_list),\n]\n",
            ),
            (
                "shop/views.py",
                "def product_list(request):\n    return None\n",
            ),
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.starts_with("shop/urls.py:shop/urls.py::route:products/ -[references]-> shop/views.py:product_list")),
            "{expected:#?}"
        );
        let row_key = crate::control_inputs::ControlKind::Python.row_key();
        let row = metadata(&db, &row_key).expect("the Python row");
        let moved = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::control_inputs::check(&store, dir.path(), false, false, &[], &[])
                .unwrap()
                .changed
                .contains(&crate::control_inputs::ControlKind::Python)
        };
        assert!(!moved(&db));
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(&row_key)
            .unwrap();
        assert!(moved(&db));
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "Django was detected before");
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_KEY),
            Some("[\"django\"]".to_string())
        );
        assert!(!moved(&db));
    }

    /// W7-19c: likewise for the `jvm` kind. The first sync after the row is
    /// gone detects the frameworks once (Spring, as before), writes the row
    /// back and leaves the graph as it was; the next does not count it.
    #[test]
    fn a_missing_jvm_control_row_detects_the_frameworks_once() {
        let dir = TestDir::new("control-inputs-jvm-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            (
                "pom.xml",
                "<project><dependency>spring-boot-starter-web</dependency></project>\n",
            ),
            (
                "src/main/java/demo/HelloController.java",
                "package demo;\n\n@RestController\npublic class HelloController {\n    @GetMapping(\"/hello\")\n    public String hello() {\n        return \"hi\";\n    }\n}\n",
            ),
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(
            expected.iter().any(|edge| edge.starts_with("src/main/java/demo/HelloController.java:")
                && edge.contains("-[references]-> src/main/java/demo/HelloController.java:demo::HelloController::hello")),
            "{expected:#?}"
        );
        let row_key = crate::control_inputs::ControlKind::Jvm.row_key();
        let row = metadata(&db, &row_key).expect("the JVM row");
        let moved = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::control_inputs::check(&store, dir.path(), false, false, &[], &[])
                .unwrap()
                .changed
                .contains(&crate::control_inputs::ControlKind::Jvm)
        };
        assert!(!moved(&db));
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(&row_key)
            .unwrap();
        assert!(moved(&db));
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "Spring was detected before");
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_KEY),
            Some("[\"spring\"]".to_string())
        );
        assert!(!moved(&db));
    }

    /// W7-19d: likewise for the `php` kind, in an index built before it
    /// and the Laravel and Drupal lanes. The first sync detects the
    /// frameworks once (Laravel and Drupal, as before), re-resolves every
    /// route file's references once, bringing back the edges an older sync
    /// lost, and writes the row and both markers back; the next counts
    /// neither.
    #[test]
    fn a_missing_php_control_row_and_lane_markers_detect_and_re_resolve_once() {
        let dir = TestDir::new("control-inputs-php-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            LARAVEL_ARTISAN,
            LARAVEL_ROUTES,
            LARAVEL_INDEX,
            DRUPAL_COMPOSER,
            DRUPAL_ROUTING,
            DRUPAL_METHOD,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.starts_with("routes/web.php:")
                    && edge.contains(&format!("-[references]-> {}:", LARAVEL_INDEX.0))),
            "{expected:#?}"
        );
        assert!(
            expected
                .iter()
                .any(|edge| edge.starts_with(&format!("{}:", DRUPAL_ROUTING.0))
                    && edge.contains("GreetingController::hello ")),
            "{expected:#?}"
        );
        let row_key = crate::control_inputs::ControlKind::Php.row_key();
        let row = metadata(&db, &row_key).expect("the PHP row");
        let frameworks = metadata(&db, crate::control_inputs::FRAMEWORKS_KEY);
        assert!(
            frameworks
                .as_deref()
                .is_some_and(|found| found.contains("\"laravel\"") && found.contains("\"drupal\"")),
            "{frameworks:?}"
        );
        let moved = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::control_inputs::check(&store, dir.path(), false, false, &[], &[])
                .unwrap()
                .changed
                .contains(&crate::control_inputs::ControlKind::Php)
        };
        let lanes_pending = |db: &Path| {
            let store = Store::open(db).unwrap();
            (
                crate::laravel_routes::needs_bootstrap(&store).unwrap(),
                crate::drupal_refs::needs_bootstrap(&store).unwrap(),
            )
        };
        assert!(!moved(&db));
        assert_eq!(lanes_pending(&db), (false, false));
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 2, "the route edges an older sync lost");
            store.delete_project_metadata(&row_key).unwrap();
            store
                .delete_project_metadata(crate::laravel_routes::VERSION_KEY)
                .unwrap();
            store
                .delete_project_metadata(crate::drupal_refs::VERSION_KEY)
                .unwrap();
        }
        assert!(moved(&db));
        assert_eq!(lanes_pending(&db), (true, true));
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            outcome.files_reindexed, 0,
            "Laravel and Drupal were detected before"
        );
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_KEY),
            frameworks
        );
        assert!(!moved(&db));
        assert_eq!(lanes_pending(&db), (false, false));
    }

    /// W7-19e: likewise for the `ruby` kind, in an index built before it
    /// and the Rails lane. The first sync detects the frameworks once (Rails,
    /// as before), re-resolves every route file's references once, bringing
    /// back the edge an older sync lost, and writes the row and the marker
    /// back; the next counts neither.
    #[test]
    fn a_missing_ruby_control_row_and_lane_marker_detect_and_re_resolve_once() {
        let dir = TestDir::new("control-inputs-ruby-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RAILS_GEMFILE, RAILS_ROUTES, RAILS_INDEX];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.starts_with(&format!("{}:", RAILS_ROUTES.0))
                    && edge.contains(&format!("-[references]-> {}:", RAILS_INDEX.0))),
            "{expected:#?}"
        );
        let row_key = crate::control_inputs::ControlKind::Ruby.row_key();
        let row = metadata(&db, &row_key).expect("the Ruby row");
        let frameworks = metadata(&db, crate::control_inputs::FRAMEWORKS_KEY);
        assert!(
            frameworks
                .as_deref()
                .is_some_and(|found| found.contains("\"rails\"")),
            "{frameworks:?}"
        );
        let moved = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::control_inputs::check(&store, dir.path(), false, false, &[], &[])
                .unwrap()
                .changed
                .contains(&crate::control_inputs::ControlKind::Ruby)
        };
        let lane_pending = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::rails_routes::needs_bootstrap(&store).unwrap()
        };
        assert!(!moved(&db));
        assert!(!lane_pending(&db));
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the route edge an older sync lost");
            store.delete_project_metadata(&row_key).unwrap();
            store
                .delete_project_metadata(crate::rails_routes::VERSION_KEY)
                .unwrap();
        }
        assert!(moved(&db));
        assert!(lane_pending(&db));
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "Rails was detected before");
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_KEY),
            frameworks
        );
        assert!(!moved(&db));
        assert!(!lane_pending(&db));
    }

    /// W7-19g: likewise for the `swift` kind, in an index built before it
    /// and the Vapor lane. The first sync detects the frameworks once (Vapor,
    /// as before), re-resolves every route file's references once, bringing
    /// back the edge an older sync lost, and writes the row and the marker
    /// back; the next counts neither.
    #[test]
    fn a_missing_swift_control_row_and_lane_marker_detect_and_re_resolve_once() {
        let dir = TestDir::new("control-inputs-swift-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [VAPOR_PACKAGE, VAPOR_ROUTES, VAPOR_REQUEST];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(vapor_routed(&db), "{expected:#?}");
        let row_key = crate::control_inputs::ControlKind::Swift.row_key();
        let row = metadata(&db, &row_key).expect("the Swift row");
        let frameworks = metadata(&db, crate::control_inputs::FRAMEWORKS_KEY);
        assert!(
            frameworks
                .as_deref()
                .is_some_and(|found| found.contains("\"vapor\"")),
            "{frameworks:?}"
        );
        let moved = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::control_inputs::check(&store, dir.path(), false, false, &[], &[])
                .unwrap()
                .changed
                .contains(&crate::control_inputs::ControlKind::Swift)
        };
        let lane_pending = |db: &Path| {
            let store = Store::open(db).unwrap();
            crate::vapor_routes::needs_bootstrap(&store).unwrap()
        };
        assert!(!moved(&db));
        assert!(!lane_pending(&db));
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the route edge an older sync lost");
            store.delete_project_metadata(&row_key).unwrap();
            store
                .delete_project_metadata(crate::vapor_routes::VERSION_KEY)
                .unwrap();
        }
        assert!(moved(&db));
        assert!(lane_pending(&db));
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(outcome.files_reindexed, 0, "Vapor was detected before");
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(metadata(&db, &row_key), Some(row));
        assert_eq!(
            metadata(&db, crate::control_inputs::FRAMEWORKS_KEY),
            frameworks
        );
        assert!(!moved(&db));
        assert!(!lane_pending(&db));
    }

    /// W5-06: a control file a watcher event names is read whole, so one
    /// replaced with its size and modification time kept still counts as
    /// changed.
    #[test]
    fn a_named_control_file_replaced_with_its_size_and_mtime_kept_is_compared() {
        let dir = TestDir::new("control-inputs-same-stat");
        let db = default_db_path(dir.path()).unwrap();
        // The same length as TS_CONFIG_PATHS, aliasing another folder.
        let elsewhere = (
            "tsconfig.json",
            "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/xyz/*\"]}}}\n",
        );
        assert_eq!(elsewhere.1.len(), TS_CONFIG_PATHS.1.len());
        let initial = [elsewhere, TS_MATH, TS_MAIN];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert!(!imports_double(&db), "{:#?}", edge_projection(&db));
        let config = dir.path().join("tsconfig.json");
        let modified = fs::metadata(&config).unwrap().modified().unwrap();
        fs::write(&config, TS_CONFIG_PATHS.1).unwrap();
        fs::File::options()
            .write(true)
            .open(&config)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        sync_changed_paths(dir.path(), &db, ["tsconfig.json"]).unwrap();
        assert!(imports_double(&db), "{:#?}", edge_projection(&db));
    }

    /// W5-06: a control file is compared by its whole text whatever its size:
    /// a `tsconfig.json` past 64 MiB replaced with its size and modification
    /// time kept moves the alias for a full sync and for a watcher's sync of
    /// the path alike, as a fresh index of the new text has it.
    #[test]
    fn a_large_control_file_replaced_with_its_size_and_mtime_kept_is_compared() {
        let padded = |target: &str| {
            format!(
                "{{\"compilerOptions\":{{\"baseUrl\":\".\",\"paths\":{{\"@lib/*\":[\"{target}\"]}}}}}}{}\n",
                " ".repeat(64 * 1024 * 1024 + 1)
            )
        };
        let (before, after) = (padded("src/xyz/*"), padded("src/lib/*"));
        assert_eq!(before.len(), after.len());
        let fresh = {
            let files = [("tsconfig.json", after.as_str()), TS_MATH, TS_MAIN];
            fresh_edges("control-inputs-large-fresh", &files)
        };
        for watcher in [false, true] {
            let dir = TestDir::new(&format!("control-inputs-large-{watcher}"));
            let db = default_db_path(dir.path()).unwrap();
            let initial = [("tsconfig.json", before.as_str()), TS_MATH, TS_MAIN];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert!(!imports_double(&db), "{:#?}", edge_projection(&db));
            let config = dir.path().join("tsconfig.json");
            let modified = fs::metadata(&config).unwrap().modified().unwrap();
            fs::write(&config, &after).unwrap();
            fs::File::options()
                .write(true)
                .open(&config)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            if watcher {
                sync_changed_paths(dir.path(), &db, ["tsconfig.json"]).unwrap();
            } else {
                sync_project_once(dir.path()).unwrap();
            }
            assert!(imports_double(&db), "watcher {watcher}");
            assert_eq!(edge_projection(&db), fresh, "watcher {watcher}");
        }
    }

    /// W5-06: a directory coming into a workspace glob's listing moves the
    /// listing, and so does the listed folder coming whole, though no event
    /// names a control file in either (each is moved in whole): the members'
    /// manifests are read, and the imports of their packages resolve as a
    /// fresh index resolves them.
    #[test]
    fn a_member_directory_coming_into_a_workspace_glob_is_compared() {
        let dir = TestDir::new("control-inputs-member-dir");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            (
                "package.json",
                "{\"name\":\"root\",\"workspaces\":[\"packages/*\",\"apps/*\"]}\n",
            ),
            ("packages/core/package.json", "{\"name\":\"@acme/core\"}\n"),
            ("src/button.ts", "export class Button {}\n"),
            (
                "src/app.ts",
                "import { Button } from '@acme/ui';\n\nexport const b = new Button();\n",
            ),
            (
                "src/web.ts",
                "import { Button } from '@acme/web';\n\nexport const w = new Button();\n",
            ),
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let imports_button = |db: &Path, from: &str| {
            let edge = format!("{from} -[imports]-> src/button.ts:Button");
            edge_projection(db).iter().any(|e| e.contains(&edge))
        };
        assert!(
            !imports_button(&db, "src/app.ts"),
            "{:#?}",
            edge_projection(&db)
        );
        assert!(
            !imports_button(&db, "src/web.ts"),
            "{:#?}",
            edge_projection(&db)
        );
        let staging = TestDir::new("control-inputs-member-dir-staging");
        let mut fresh = initial.to_vec();

        let member = ("packages/ui/package.json", "{\"name\":\"@acme/ui\"}\n");
        write_files(staging.path(), &[("ui/package.json", member.1)]);
        fs::rename(staging.path().join("ui"), dir.path().join("packages/ui")).unwrap();
        sync_changed_paths(dir.path(), &db, ["packages/ui"]).unwrap();
        fresh.push(member);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("control-inputs-member-dir-fresh", &fresh)
        );
        assert!(
            imports_button(&db, "src/app.ts"),
            "{:#?}",
            edge_projection(&db)
        );

        let listed = ("apps/web/package.json", "{\"name\":\"@acme/web\"}\n");
        write_files(staging.path(), &[listed]);
        fs::rename(staging.path().join("apps"), dir.path().join("apps")).unwrap();
        sync_changed_paths(dir.path(), &db, ["apps"]).unwrap();
        fresh.push(listed);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("control-inputs-listed-dir-fresh", &fresh)
        );
        assert!(
            imports_button(&db, "src/web.ts"),
            "{:#?}",
            edge_projection(&db)
        );
    }

    /// W5-06: a control file's change re-resolves its kind only when a reader
    /// reads it. A manifest's `version` bump does (fail closed: its digest
    /// moved), so the import edge an older sync lost comes back; a manifest
    /// in a folder no reader reads is no input, so nothing re-resolves and
    /// the row stays.
    #[test]
    fn a_control_file_reresolves_its_kind_only_when_a_reader_reads_it() {
        let dir = TestDir::new("control-inputs-unread");
        let db = default_db_path(dir.path()).unwrap();
        let manifest = ("package.json", "{\"name\":\"web\",\"version\":\"1.0.0\"}\n");
        let initial = [manifest, TS_CONFIG_PATHS, TS_MATH, TS_MAIN];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(imports_double(&db), "{expected:#?}");
        let lose_the_import = || {
            let dropped = Store::open(&db)
                .unwrap()
                .connection()
                .execute(DROP_DOUBLE_IMPORT, [])
                .unwrap();
            assert_eq!(dropped, 1, "the import edge an older sync lost");
        };

        lose_the_import();
        write_files(
            dir.path(),
            &[("package.json", "{\"name\":\"web\",\"version\":\"1.0.1\"}\n")],
        );
        sync_changed_paths(dir.path(), &db, ["package.json"]).unwrap();
        assert_eq!(edge_projection(&db), expected, "every JS file re-resolves");

        lose_the_import();
        let row_key = crate::control_inputs::ControlKind::Js.row_key();
        let row = metadata(&db, &row_key);
        write_files(
            dir.path(),
            &[("docs/package.json", "{\"name\":\"docs\"}\n")],
        );
        let outcome = sync_changed_paths(dir.path(), &db, ["docs/package.json"]).unwrap();
        assert!(
            !imports_double(&db),
            "nothing re-resolves: {:#?}",
            edge_projection(&db)
        );
        assert_eq!(metadata(&db, &row_key), row, "the manifest is no input");
        let recorded = outcome.control_paths.expect("the recorded inputs");
        assert!(recorded.may_move("package.json", false));
        assert!(!recorded.may_move("docs/package.json", false));
    }

    fn case_folded_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::case_folded::VERSION_KEY)
            .unwrap()
    }

    const PAS_VIEW: (&str, &str) = (
        "View.pas",
        "unit View;\ninterface\nfunction Render: Integer;\nimplementation\nfunction Render: Integer;\nbegin\n  Result := formatprice(10);\nend;\nend.\n",
    );
    const PAS_PRICES: (&str, &str) = (
        "Prices.pas",
        "unit Prices;\ninterface\nimplementation\nfunction FormatPrice(Amount: Integer): Integer;\nbegin\n  Result := Amount;\nend;\nend.\n",
    );
    const PAS_SPARE: (&str, &str) = (
        "Spare.pas",
        "unit Spare;\ninterface\nimplementation\nend.\n",
    );
    const PAS_SPARE_EDITED: (&str, &str) = (
        "Spare.pas",
        "unit Spare;\ninterface\nimplementation\nprocedure Idle;\nbegin\nend;\nend.\n",
    );

    /// W4-PHP-03: a sync failing after its mutations leaves the case-folded
    /// marker away, so the next sync, with nothing changed or with an
    /// unrelated change, re-resolves every PHP, Pascal and CFML file: the
    /// Pascal `formatprice(10)` reaches the `FormatPrice` the failed sync
    /// indexed, as a fresh index has it.
    #[test]
    fn a_case_folded_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_price = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("-[calls]-> Prices.pas:FormatPrice"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "case-folded-fail-unrelated"
            } else {
                "case-folded-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [PAS_VIEW, PAS_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                case_folded_marker(&db),
                Some(crate::case_folded::VERSION.to_string())
            );
            assert!(!to_price(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[PAS_PRICES]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [PAS_PRICES.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                case_folded_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[PAS_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [PAS_SPARE.0]).unwrap();
                PAS_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                PAS_SPARE
            };
            assert_eq!(
                case_folded_marker(&db),
                Some(crate::case_folded::VERSION.to_string())
            );
            let fresh = [PAS_VIEW, PAS_PRICES, spare];
            let name = if unrelated {
                "case-folded-fresh-unrelated"
            } else {
                "case-folded-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_price(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W4-PHP-03: an index whose case-folded marker alone is gone has every
    /// PHP, Pascal and CFML file re-resolved by its next sync, even one with
    /// nothing changed: the case-folded call an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_case_folded_marker_re_resolves_with_nothing_changed() {
        let version = Some(crate::case_folded::VERSION.to_string());
        let dir = TestDir::new("case-folded-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [PAS_VIEW, PAS_PRICES, PAS_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(case_folded_marker(&db), version);
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.contains("-[calls]-> Prices.pas:FormatPrice")),
            "{expected:#?}"
        );
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'FormatPrice')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata(crate::case_folded::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            php_marker(&db),
            Some(crate::php_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(case_folded_marker(&db), version);
        assert_eq!(edge_projection(&db), expected);
    }

    fn lua_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::lua_modules::VERSION_KEY)
            .unwrap()
    }

    fn lua_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::lua_scope::MODULE_ROW_KEY_PREFIX)
            .unwrap()
    }

    const LUA_PHASES: (&str, &str) = (
        "kong/pdk/phases.lua",
        "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n}\n",
    );
    const LUA_PHASES_EXPORTING: (&str, &str) = (
        "kong/pdk/phases.lua",
        "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
    );
    const LUA_HANDLER: (&str, &str) = (
        "kong/handler.lua",
        "local phases = require \"kong.pdk.phases\"\nlocal cp = phases.check\n\nlocal function run()\n  cp(\"access\")\nend\n\nreturn { run = run }\n",
    );
    const LUA_SPARE: (&str, &str) = ("kong/spare.lua", "return {}\n");
    const LUA_SPARE_EDITED: (&str, &str) = ("kong/spare.lua", "local idle = 1\nreturn {}\n");

    /// W4-LUA-01: a sync failing after its mutations leaves the Lua marker
    /// away, so the next sync, with nothing changed or with an unrelated
    /// change, re-resolves every Lua file: the call through `local cp =
    /// phases.check` reaches what the failed sync's module started handing
    /// out, as a fresh index has it.
    #[test]
    fn a_lua_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_check = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("-[calls]-> kong/pdk/phases.lua:check_phase"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "lua-modules-fail-unrelated"
            } else {
                "lua-modules-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [LUA_PHASES, LUA_HANDLER, LUA_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                lua_marker(&db),
                Some(crate::lua_modules::VERSION.to_string())
            );
            assert!(!to_check(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[LUA_PHASES_EXPORTING]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [LUA_PHASES.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                lua_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[LUA_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [LUA_SPARE.0]).unwrap();
                LUA_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                LUA_SPARE
            };
            assert_eq!(
                lua_marker(&db),
                Some(crate::lua_modules::VERSION.to_string())
            );
            let fresh = [LUA_PHASES_EXPORTING, LUA_HANDLER, spare];
            let name = if unrelated {
                "lua-modules-fresh-unrelated"
            } else {
                "lua-modules-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_check(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W4-LUA-01: an index whose Lua marker and rows alone are gone has every
    /// Lua file re-resolved and every row rebuilt by its next sync, even one
    /// with nothing changed: the alias call an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_lua_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::lua_modules::VERSION.to_string());
        let dir = TestDir::new("lua-modules-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LUA_PHASES_EXPORTING, LUA_HANDLER, LUA_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lua_marker(&db), version);
        let rows = lua_rows(&db);
        assert_eq!(rows.len(), 3, "{rows:#?}");
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'check_phase')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::lua_scope::MODULE_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::lua_modules::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            php_marker(&db),
            Some(crate::php_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lua_marker(&db), version);
        assert_eq!(lua_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-LUA-01: an index whose Lua marker is current but one of whose rows
    /// cannot be read has every Lua file re-resolved and every row rebuilt by
    /// its next sync, even one with nothing changed.
    #[test]
    fn an_index_with_an_unreadable_lua_row_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::lua_modules::VERSION.to_string());
        let dir = TestDir::new("lua-modules-unreadable-row");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LUA_PHASES_EXPORTING, LUA_HANDLER, LUA_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lua_marker(&db), version);
        let rows = lua_rows(&db);
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'check_phase')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .set_project_metadata(
                    &codegraph_resolve::lua_scope::module_row_key(LUA_HANDLER.0),
                    "{not a row",
                )
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lua_marker(&db), version);
        assert_eq!(lua_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-LUA-01: a rebuild for an unreadable row starts with the Lua marker
    /// still current and withdraws it before it changes anything, so one
    /// stopping once the old rows are purged and only one is written back
    /// leaves the next sync, with nothing changed, to rebuild them all and
    /// re-resolve every Lua file.
    #[test]
    fn a_lua_row_rebuild_stopping_partway_leaves_the_next_sync_to_finish_it() {
        let version = Some(crate::lua_modules::VERSION.to_string());
        let dir = TestDir::new("lua-modules-rebuild-partway");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LUA_PHASES_EXPORTING, LUA_HANDLER, LUA_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let rows = lua_rows(&db);
        assert_eq!(rows.len(), 3, "{rows:#?}");
        let expected = edge_projection(&db);
        Store::open(&db)
            .unwrap()
            .set_project_metadata(
                &codegraph_resolve::lua_scope::module_row_key(LUA_HANDLER.0),
                "{not a row",
            )
            .unwrap();
        FAIL_AFTER_LUA_ROW_PURGE.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_AFTER_LUA_ROW_PURGE.with(|fail| fail.set(false));
        assert!(
            failed.is_err(),
            "the injected failure must stop the rebuild"
        );
        assert_eq!(
            lua_marker(&db),
            None,
            "the rebuild withdrew the marker first"
        );
        assert_eq!(lua_rows(&db).len(), 1, "{:#?}", lua_rows(&db));
        let dropped = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                 WHERE name = 'check_phase')",
                [],
            )
            .unwrap();
        assert_eq!(dropped, 1, "the call edge a later loss takes");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lua_marker(&db), version);
        assert_eq!(lua_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn lua_loaders_record(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::lua_modules::LOADERS_KEY)
            .unwrap()
    }

    const LUA_LOADED: (&str, &str) = (
        "Script/Foo.lua",
        "local M = {}\nfunction M.run()\n  return 1\nend\nreturn M\n",
    );
    const LUA_LOADER_CALLER: (&str, &str) = ("main.lua", "Require(\"Script.Foo\")\nreturn {}\n");
    const LUA_LOADER_CONFIG: (&str, &str) = (
        ".codegraph/config.toml",
        "[app]\nname = \"lua-loaders\"\n\n[lua]\nloader_functions = [\"Require\"]\n",
    );

    /// W9-02: a sync for new Lua loader settings withdraws the record before
    /// it changes anything, so one failing once every Lua file is extracted
    /// again leaves the record away, and the next sync, with nothing
    /// changed, extracts them again and converges with a fresh index.
    #[test]
    fn a_failed_loader_change_sync_leaves_the_record_withdrawn() {
        let dir = TestDir::new("lua-loaders-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LUA_LOADED, LUA_LOADER_CALLER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let unconfigured = codegraph_extract::LuaLoaders::default().canonical_json();
        assert_eq!(lua_loaders_record(&db), Some(unconfigured));
        let imports_foo = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.starts_with("main.lua:main.lua -[imports]-> Script/Foo.lua:"))
        };
        assert!(!imports_foo(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[LUA_LOADER_CONFIG]);
        FAIL_AFTER_LUA_LOADER_REEXTRACT.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_AFTER_LUA_LOADER_REEXTRACT.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lua_loaders_record(&db),
            None,
            "the sync withdrew the record before its first change"
        );

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let configured = ProjectScope::load(dir.path(), &index_paths(dir.path()).unwrap())
            .unwrap()
            .options
            .lua_loaders
            .canonical_json();
        assert_eq!(lua_loaders_record(&db), Some(configured));
        assert!(imports_foo(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "lua-loaders-fail-fresh",
                &[LUA_LOADER_CONFIG, LUA_LOADED, LUA_LOADER_CALLER]
            )
        );
    }

    /// W9-02: a project naming no Lua loader records that too, so its next
    /// sync extracts only what changed, never every Lua file.
    #[test]
    fn a_lua_project_without_settings_does_not_reextract_on_sync() {
        let dir = TestDir::new("lua-loaders-none");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LUA_PHASES_EXPORTING, LUA_HANDLER, LUA_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lua_loaders_record(&db),
            Some(codegraph_extract::LuaLoaders::default().canonical_json())
        );
        write_files(dir.path(), &[LUA_SPARE_EDITED]);
        let outcome = sync_changed_paths(dir.path(), &db, [LUA_SPARE.0]).unwrap();
        assert_eq!(outcome.changed_paths, vec![LUA_SPARE.0.to_string()]);
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(
            outcome.changed_paths.is_empty(),
            "{:?}",
            outcome.changed_paths
        );
    }

    const LUA_LIB_UTIL: (&str, &str) = (
        "lib/util.lua",
        "local M = {}\nfunction M.clamp(v)\n  return v\nend\nreturn M\n",
    );
    const LUA_SCRIPT_UTIL: (&str, &str) = (
        "Script/util.lua",
        "local M = {}\nfunction M.clamp(v)\n  return v + 1\nend\nreturn M\n",
    );
    const LUA_ALIAS_READER: (&str, &str) = (
        "main.lua",
        "local clamp = require(\"util\").clamp\n\nlocal function run()\n  return clamp(1)\nend\n\nreturn run\n",
    );
    const LUA_PLAIN_REQUIRE: (&str, &str) = ("plain.lua", "require(\"util\")\nreturn {}\n");
    const LUA_LIB_ROOT: (&str, &str) = (
        ".codegraph/config.toml",
        "[app]\nname = \"lua-roots\"\n\n[lua]\nmodule_roots = [\"lib\"]\n",
    );

    /// W9-02: a change of the Lua module roots alone extracts nothing
    /// again: each Lua import goes back by its site, and each Lua file whose
    /// aliases `require` a module goes back whole, so the call through the
    /// alias follows the import to the root's module, as a fresh index has
    /// it.
    #[test]
    fn a_root_change_reresolves_imports_by_site_and_alias_readers_whole() {
        let dir = TestDir::new("lua-roots-change");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            LUA_LIB_UTIL,
            LUA_SCRIPT_UTIL,
            LUA_ALIAS_READER,
            LUA_PLAIN_REQUIRE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let reaches = |db: &Path, file: &str| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.starts_with("main.lua:run -[calls]-> ") && edge.contains(file))
        };
        assert!(
            reaches(&db, "Script/util.lua:"),
            "{:#?}",
            edge_projection(&db)
        );
        take_dependent_refreshes();

        write_files(dir.path(), &[LUA_LIB_ROOT]);
        let outcome = sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(
            outcome.changed_paths.is_empty(),
            "{:?}",
            outcome.changed_paths
        );
        let refreshed: Vec<(String, bool)> = take_dependent_refreshes()
            .into_iter()
            .map(|refresh| (refresh.file, refresh.whole))
            .collect();
        assert_eq!(
            refreshed,
            vec![
                ("main.lua".to_string(), true),
                ("plain.lua".to_string(), false),
            ]
        );
        assert!(reaches(&db, "lib/util.lua:"), "{:#?}", edge_projection(&db));
        let mut fresh = vec![LUA_LIB_ROOT];
        fresh.extend(initial);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("lua-roots-change-fresh", &fresh)
        );
    }

    /// W9-02: the orphan sweep resolves under the project's Lua module
    /// roots, as the full index it heals does.
    #[test]
    fn the_orphan_sweep_reads_the_lua_roots() {
        let dir = TestDir::new("lua-roots-sweep");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            LUA_LIB_ROOT,
            LUA_LIB_UTIL,
            LUA_SCRIPT_UTIL,
            LUA_PLAIN_REQUIRE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let expected = edge_projection(&db);
        assert!(
            expected
                .iter()
                .any(|edge| edge.starts_with("plain.lua:plain.lua -[imports]-> lib/util.lua:")),
            "{expected:#?}"
        );
        {
            // An interrupted resolution: the import parked again, its edge
            // gone.
            let store = Store::open(&db).unwrap();
            let conn = store.connection();
            let moved = conn
                .execute(
                    "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, \
                     line, col, candidates, file_path, language, reference_subkind) \
                     SELECT from_node_id, reference_name, reference_kind, line, col, candidates, \
                     file_path, language, reference_subkind FROM resolved_refs \
                     WHERE file_path = 'plain.lua' AND reference_kind = 'imports'",
                    [],
                )
                .unwrap();
            assert_eq!(moved, 1);
            conn.execute(
                "DELETE FROM resolved_refs WHERE file_path = 'plain.lua' AND reference_kind = 'imports'",
                [],
            )
            .unwrap();
            let dropped = conn
                .execute(
                    "DELETE FROM edges WHERE kind = 'imports' AND source = 'file:plain.lua'",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1);
            store.set_resolution_incomplete().unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(
            !Store::open(&db)
                .unwrap()
                .is_resolution_incomplete()
                .unwrap()
        );
        assert_eq!(edge_projection(&db), expected);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("lua-roots-sweep-fresh", &initial)
        );
    }

    fn r_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::r_bindings::VERSION_KEY)
            .unwrap()
    }

    const R_DATA: (&str, &str) = ("R/data.R", "c <- list(f = 1)\n");
    const R_DATA_FUNCTION: (&str, &str) = ("R/data.R", "c <- list(f = function(x) x)\n");
    const R_USE: (&str, &str) = ("R/use.R", "use <- function() {\n  c(1, 2)\n}\n");
    const R_SPARE: (&str, &str) = ("R/spare.R", "spare <- function() 1\n");
    const R_SPARE_EDITED: (&str, &str) = ("R/spare.R", "spare <- function() 2\n");

    /// W4-R-01: a sync failing after it wrote `c <- list(f = function(x)
    /// x)`, before resolving, leaves the next sync, with nothing changed or
    /// with an unrelated change, to bring the call in another file to `c`, as
    /// a fresh index has it: no node of `c` changed, and its line is what
    /// the data-binding rule reads (upstream #2239).
    #[test]
    fn an_r_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_c = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("-[calls]-> R/data.R:c"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "r-bindings-fail-unrelated"
            } else {
                "r-bindings-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [R_DATA, R_USE, R_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(r_marker(&db), Some(crate::r_bindings::VERSION.to_string()));
            assert!(!to_c(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[R_DATA_FUNCTION]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [R_DATA.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(r_marker(&db), None, "a failed sync leaves the marker away");

            let spare = if unrelated {
                write_files(dir.path(), &[R_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [R_SPARE.0]).unwrap();
                R_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                R_SPARE
            };
            let fresh = [R_DATA_FUNCTION, R_USE, spare];
            let name = if unrelated {
                "r-bindings-fresh-unrelated"
            } else {
                "r-bindings-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_c(&db), "{:#?}", edge_projection(&db));
            assert_eq!(r_marker(&db), Some(crate::r_bindings::VERSION.to_string()));
        }
    }

    /// W4-R-01: an index whose R marker alone is gone has every R file
    /// re-resolved by its next sync, even one with nothing changed: the call
    /// an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_r_marker_re_resolves_with_nothing_changed() {
        let version = Some(crate::r_bindings::VERSION.to_string());
        let dir = TestDir::new("r-bindings-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [R_DATA_FUNCTION, R_USE, R_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(r_marker(&db), version);
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE name = 'c')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata(crate::r_bindings::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            lua_marker(&db),
            Some(crate::lua_modules::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(r_marker(&db), version);
        assert_eq!(edge_projection(&db), expected);
    }

    fn play_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::play_routes::VERSION_KEY)
            .unwrap()
    }

    const PLAY_ROUTES: (&str, &str) = ("conf/routes", "GET / controllers.HomeController.index\n");
    const PLAY_FAR: (&str, &str) = (
        "app/controllers/HomeController.scala",
        "package controllers\n\nclass HomeController {\n  def index(): Int = 1\n}\n",
    );
    const PLAY_NEAR: (&str, &str) = (
        "conf/HomeController.scala",
        "package conf\n\nclass HomeController {\n  def index(): Int = 2\n}\n",
    );

    /// W7-19c: a sync failing after it indexed a nearer `HomeController`,
    /// before resolving, leaves the next sync, with nothing changed, to move
    /// the routes file's action to it, as a fresh index has it: the routes
    /// file is a YAML file no other lane's bootstrap re-resolves, and the
    /// Play marker the failure left away brings it back.
    #[test]
    fn a_play_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to = |db: &Path, file: &str| {
            edge_projection(db).iter().any(|edge| {
                edge.starts_with("conf/routes:")
                    && edge.contains(&format!("-[references]-> {file}:"))
            })
        };
        let dir = TestDir::new("play-routes-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [PLAY_ROUTES, PLAY_FAR];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            play_marker(&db),
            Some(crate::play_routes::VERSION.to_string())
        );
        assert!(to(&db, PLAY_FAR.0), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[PLAY_NEAR]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [PLAY_NEAR.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            play_marker(&db),
            None,
            "a failed sync leaves the marker away"
        );
        // The lane refreshed the routes file's site before the failure: its
        // reference waits unresolved.
        assert!(!to(&db, PLAY_NEAR.0), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [PLAY_ROUTES, PLAY_FAR, PLAY_NEAR];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("play-routes-fresh", &fresh)
        );
        assert!(to(&db, PLAY_NEAR.0), "{:#?}", edge_projection(&db));
        assert_eq!(
            play_marker(&db),
            Some(crate::play_routes::VERSION.to_string())
        );
    }

    const SPRING_APP: (&str, &str) = (
        "src/main/java/demo/App.java",
        "package demo;\n\n@RestController\nclass App {\n  @Value(\"${app.name}\")\n  private String name;\n}\n",
    );
    const SPRING_CONFIG: (&str, &str) =
        ("src/main/resources/application.yml", "app:\n  other: x\n");
    const SPRING_CONFIG_NAMED: (&str, &str) = (
        "src/main/resources/application.yml",
        "app:\n  other: x\n  name: demo\n",
    );

    /// W7-19c: a sync failing after it wrote a config key, before resolving,
    /// leaves the next sync, with nothing changed, to bind the `@Value` to
    /// it, as a fresh index has it: the bindings are Java references, which
    /// the Java bootstrap the failure left behind re-resolves; the Play
    /// marker goes and comes back with the others.
    #[test]
    fn a_spring_config_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let bound = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.starts_with("src/main/java/demo/App.java:")
                    && edge.contains("-[references]-> src/main/resources/application.yml:app.name ")
            })
        };
        let dir = TestDir::new("spring-config-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [SPRING_APP, SPRING_CONFIG];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            play_marker(&db),
            Some(crate::play_routes::VERSION.to_string())
        );
        assert!(!bound(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[SPRING_CONFIG_NAMED]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [SPRING_CONFIG.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            play_marker(&db),
            None,
            "a failed sync leaves the marker away"
        );
        assert!(!bound(&db), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [SPRING_APP, SPRING_CONFIG_NAMED];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("spring-config-fresh", &fresh)
        );
        assert!(bound(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            play_marker(&db),
            Some(crate::play_routes::VERSION.to_string())
        );
    }

    fn lane_marker(db: &Path, key: &str) -> Option<String> {
        Store::open(db).unwrap().get_project_metadata(key).unwrap()
    }

    const LARAVEL_ARTISAN: (&str, &str) = ("artisan", "#!/usr/bin/env php\n<?php\n");
    const LARAVEL_ROUTES: (&str, &str) = (
        "routes/web.php",
        "<?php\n\nRoute::get('/users', 'Admin\\UserController@index');\n",
    );
    const LARAVEL_EMPTY: (&str, &str) = (
        "app/Http/Controllers/Admin/UserController.php",
        "<?php\n\nclass UserController\n{\n}\n",
    );
    const LARAVEL_INDEX: (&str, &str) = (
        "app/Http/Controllers/Admin/UserController.php",
        "<?php\n\nclass UserController\n{\n    public function index()\n    {\n    }\n}\n",
    );

    /// W7-19d: a sync failing after it gave the controller its `index`,
    /// before resolving, leaves the next sync, with nothing changed, to link
    /// the route's `Admin\UserController@index` to it, as a fresh index has
    /// it: the reference waits under its whole name, which no lane but
    /// Laravel's wakes, and the Laravel marker the failure left away brings
    /// it back.
    #[test]
    fn a_laravel_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let routed = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.starts_with("routes/web.php:")
                    && edge.contains(&format!("-[references]-> {}:", LARAVEL_INDEX.0))
            })
        };
        let dir = TestDir::new("laravel-routes-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LARAVEL_ARTISAN, LARAVEL_ROUTES, LARAVEL_EMPTY];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::laravel_routes::VERSION_KEY),
            Some(crate::laravel_routes::VERSION.to_string())
        );
        assert!(!routed(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[LARAVEL_INDEX]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [LARAVEL_INDEX.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::laravel_routes::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );
        assert!(!routed(&db), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [LARAVEL_ARTISAN, LARAVEL_ROUTES, LARAVEL_INDEX];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("laravel-routes-fresh", &fresh)
        );
        assert!(routed(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            lane_marker(&db, crate::laravel_routes::VERSION_KEY),
            Some(crate::laravel_routes::VERSION.to_string())
        );
    }

    /// W7-19d: an index without the Laravel marker (built before it, or a
    /// sync that stopped) has every file holding a Laravel route's reference
    /// re-resolved by its next sync, with nothing changed: the controller
    /// edge an older sync lost comes back.
    #[test]
    fn an_index_without_the_laravel_marker_re_resolves_the_route_files() {
        let version = Some(crate::laravel_routes::VERSION.to_string());
        let dir = TestDir::new("laravel-routes-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LARAVEL_ARTISAN, LARAVEL_ROUTES, LARAVEL_INDEX];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::laravel_routes::VERSION_KEY),
            version
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route' AND file_path = 'routes/web.php')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the controller edge an older sync lost");
            store
                .delete_project_metadata(crate::laravel_routes::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            lane_marker(&db, crate::laravel_routes::VERSION_KEY),
            version
        );
        assert_eq!(edge_projection(&db), expected);
    }

    const DRUPAL_COMPOSER: (&str, &str) = (
        "composer.json",
        "{\"require\": {\"drupal/core\": \"^10\"}}\n",
    );
    const DRUPAL_ROUTING: (&str, &str) = (
        "modules/greeting/greeting.routing.yml",
        "greeting.hello:\n  path: '/hello'\n  defaults:\n    _controller: '\\Drupal\\greeting\\Controller\\GreetingController::hello'\n",
    );
    const DRUPAL_CLASS: (&str, &str) = (
        "modules/greeting/src/Controller/GreetingController.php",
        "<?php\n\nnamespace Drupal\\greeting\\Controller;\n\nclass GreetingController\n{\n}\n",
    );
    const DRUPAL_METHOD: (&str, &str) = (
        "modules/greeting/src/Controller/GreetingController.php",
        "<?php\n\nnamespace Drupal\\greeting\\Controller;\n\nclass GreetingController\n{\n    public function hello()\n    {\n    }\n}\n",
    );

    /// W7-19d: a sync failing after it gave the controller class its
    /// `hello`, before resolving, leaves the next sync, with nothing changed,
    /// to move the routing file's handler from the class to the method, as a
    /// fresh index has it: the routing file is a YAML file no other lane's
    /// bootstrap re-resolves, and the Drupal marker the failure left away
    /// brings it back.
    #[test]
    fn a_drupal_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_method = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.starts_with(&format!("{}:", DRUPAL_ROUTING.0))
                    && edge.contains("GreetingController::hello ")
            })
        };
        let dir = TestDir::new("drupal-refs-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DRUPAL_COMPOSER, DRUPAL_ROUTING, DRUPAL_CLASS];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::drupal_refs::VERSION_KEY),
            Some(crate::drupal_refs::VERSION.to_string())
        );
        assert!(!to_method(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[DRUPAL_METHOD]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [DRUPAL_METHOD.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::drupal_refs::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [DRUPAL_COMPOSER, DRUPAL_ROUTING, DRUPAL_METHOD];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("drupal-refs-fresh", &fresh)
        );
        assert!(to_method(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            lane_marker(&db, crate::drupal_refs::VERSION_KEY),
            Some(crate::drupal_refs::VERSION.to_string())
        );
    }

    const RAILS_GEMFILE: (&str, &str) = ("Gemfile", "gem \"rails\"\n");
    const RAILS_ROUTES: (&str, &str) = (
        "config/routes.rb",
        "Rails.application.routes.draw do\n  get 'users', to: 'users#index'\nend\n",
    );
    const RAILS_EMPTY: (&str, &str) = (
        "app/controllers/users_controller.rb",
        "class UsersController\nend\n",
    );
    const RAILS_INDEX: (&str, &str) = (
        "app/controllers/users_controller.rb",
        "class UsersController\n  def index\n  end\nend\n",
    );

    /// W7-19e: a sync failing after it gave the controller its `index`,
    /// before resolving, leaves the next sync, with nothing changed, to link
    /// the route's `users#index` to it, as a fresh index has it: the
    /// reference waits under its whole name, which no lane but Rails' wakes,
    /// and the Rails marker the failure left away brings it back.
    #[test]
    fn a_rails_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let routed = |db: &Path| {
            edge_projection(db).iter().any(|edge| {
                edge.starts_with(&format!("{}:", RAILS_ROUTES.0))
                    && edge.contains(&format!("-[references]-> {}:", RAILS_INDEX.0))
            })
        };
        let dir = TestDir::new("rails-routes-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RAILS_GEMFILE, RAILS_ROUTES, RAILS_EMPTY];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::rails_routes::VERSION_KEY),
            Some(crate::rails_routes::VERSION.to_string())
        );
        assert!(!routed(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[RAILS_INDEX]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [RAILS_INDEX.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::rails_routes::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );
        assert!(!routed(&db), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [RAILS_GEMFILE, RAILS_ROUTES, RAILS_INDEX];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("rails-routes-fresh", &fresh)
        );
        assert!(routed(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            lane_marker(&db, crate::rails_routes::VERSION_KEY),
            Some(crate::rails_routes::VERSION.to_string())
        );
    }

    /// W7-19e: an index without the Rails marker (built before it, or a sync
    /// that stopped) has every file holding a Rails route's reference
    /// re-resolved by its next sync, with nothing changed: the controller
    /// edge an older sync lost comes back.
    #[test]
    fn an_index_without_the_rails_marker_re_resolves_the_route_files() {
        let version = Some(crate::rails_routes::VERSION.to_string());
        let dir = TestDir::new("rails-routes-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RAILS_GEMFILE, RAILS_ROUTES, RAILS_INDEX];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lane_marker(&db, crate::rails_routes::VERSION_KEY), version);
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route' AND file_path = 'config/routes.rb')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the controller edge an older sync lost");
            store
                .delete_project_metadata(crate::rails_routes::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lane_marker(&db, crate::rails_routes::VERSION_KEY), version);
        assert_eq!(edge_projection(&db), expected);
    }

    const VAPOR_PACKAGE: (&str, &str) = (
        "Package.swift",
        "// swift-tools-version:5.9\nlet package = Package(name: \"App\", dependencies: [.package(url: \"https://github.com/vapor/vapor.git\", from: \"4.0.0\")])\n",
    );
    const VAPOR_ROUTES: (&str, &str) = (
        "Sources/App/routes.swift",
        "import Vapor\nfunc routes(_ app: Application) throws {\n    app.get(\"sitemap\", use: SiteMapController.index)\n}\n",
    );
    const VAPOR_NO_REQUEST: (&str, &str) = (
        "Sources/App/Controllers/SiteMapController.swift",
        "import Vapor\nenum SiteMapController {\n    static func index(packages: [String]) -> String { packages.joined() }\n    static func index(names: [String]) async throws -> String { \"\" }\n}\n",
    );
    const VAPOR_REQUEST: (&str, &str) = (
        "Sources/App/Controllers/SiteMapController.swift",
        "import Vapor\nenum SiteMapController {\n    static func index(packages: [String]) -> String { packages.joined() }\n    static func index(req: Request) async throws -> String { \"\" }\n}\n",
    );

    /// Whether the Vapor route links to the handler overload that takes the
    /// `Request`.
    fn vapor_routed(db: &Path) -> bool {
        edge_projection(db).iter().any(|edge| {
            edge.starts_with(
                "Sources/App/routes.swift:Sources/App/routes.swift::route:/sitemap -[references]-> \
                 Sources/App/Controllers/SiteMapController.swift:SiteMapController::index ",
            )
        })
    }

    /// W7-19g: a sync failing after it gave the second overload its
    /// `Request`, before resolving, leaves the next sync, with nothing
    /// changed, to link the route's `SiteMapController@index` to it, as a
    /// fresh index has it: the reference waits under its whole name, which
    /// no lane but Vapor's wakes, and the Vapor marker the failure left away
    /// brings it back.
    #[test]
    fn a_vapor_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let dir = TestDir::new("vapor-routes-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [VAPOR_PACKAGE, VAPOR_ROUTES, VAPOR_NO_REQUEST];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::vapor_routes::VERSION_KEY),
            Some(crate::vapor_routes::VERSION.to_string())
        );
        assert!(!vapor_routed(&db), "{:#?}", edge_projection(&db));

        write_files(dir.path(), &[VAPOR_REQUEST]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [VAPOR_REQUEST.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::vapor_routes::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );
        assert!(!vapor_routed(&db), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [VAPOR_PACKAGE, VAPOR_ROUTES, VAPOR_REQUEST];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("vapor-routes-fresh", &fresh)
        );
        assert!(vapor_routed(&db), "{:#?}", edge_projection(&db));
        assert_eq!(
            lane_marker(&db, crate::vapor_routes::VERSION_KEY),
            Some(crate::vapor_routes::VERSION.to_string())
        );
    }

    /// W7-19g: an index without the Vapor marker (built before it, or a sync
    /// that stopped) has every file holding a Swift route's reference
    /// re-resolved by its next sync, with nothing changed: the handler edge
    /// an older sync lost comes back. The other Swift lanes' markers stay, so
    /// none of them re-resolves the route file.
    #[test]
    fn an_index_without_the_vapor_marker_re_resolves_the_route_files() {
        let version = Some(crate::vapor_routes::VERSION.to_string());
        let dir = TestDir::new("vapor-routes-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [VAPOR_PACKAGE, VAPOR_ROUTES, VAPOR_REQUEST];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lane_marker(&db, crate::vapor_routes::VERSION_KEY), version);
        assert!(vapor_routed(&db), "{:#?}", edge_projection(&db));
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'references' AND source IN (SELECT id FROM \
                     nodes WHERE kind = 'route' AND file_path = 'Sources/App/routes.swift')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the handler edge an older sync lost");
            store
                .delete_project_metadata(crate::vapor_routes::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lane_marker(&db, crate::vapor_routes::VERSION_KEY), version);
        assert_eq!(edge_projection(&db), expected);
    }

    const RUST_WORKSPACE: (&str, &str) = (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n",
    );
    const RUST_ALPHA_MANIFEST: (&str, &str) = (
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    );
    const RUST_ALPHA: (&str, &str) = ("crates/alpha/src/lib.rs", "use beta::Thing;\n");
    const RUST_BETA_MANIFEST: (&str, &str) = (
        "crates/beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    );
    const RUST_BETA_MAIN: (&str, &str) = ("crates/beta/src/main.rs", "fn main() {}\n");
    const RUST_BETA_LIB: (&str, &str) = ("crates/beta/src/lib.rs", "pub struct Thing;\n");

    /// Whether the member `alpha`'s `use beta::…` links the crate root at
    /// `root`.
    fn rust_use_links(db: &Path, root: &str) -> bool {
        edge_projection(db).iter().any(|edge| {
            edge.starts_with(&format!("{}:", RUST_ALPHA.0))
                && edge.contains(&format!("-[imports]-> {root}:"))
                && edge.contains("\"framework\":\"rust\"")
        })
    }

    /// W7-19h: a sync failing after it gave the member `beta` its
    /// `src/lib.rs`, before resolving, leaves the Rust crate roots marker
    /// away, and the next sync, with nothing changed, links `alpha`'s
    /// `use beta::…` to that `lib.rs`, as a fresh index has it. (The Rust
    /// receivers marker the failure also withdrew re-resolves every Rust file
    /// too, so the marker itself is what this test reads.)
    #[test]
    fn a_rust_crate_root_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::rust_crate_roots::VERSION.to_string());
        let dir = TestDir::new("rust-crate-roots-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            RUST_WORKSPACE,
            RUST_ALPHA_MANIFEST,
            RUST_ALPHA,
            RUST_BETA_MANIFEST,
            RUST_BETA_MAIN,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::rust_crate_roots::VERSION_KEY),
            version
        );
        assert!(
            rust_use_links(&db, RUST_BETA_MAIN.0),
            "{:#?}",
            edge_projection(&db)
        );

        write_files(dir.path(), &[RUST_BETA_LIB]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [RUST_BETA_LIB.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::rust_crate_roots::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [
            RUST_WORKSPACE,
            RUST_ALPHA_MANIFEST,
            RUST_ALPHA,
            RUST_BETA_MANIFEST,
            RUST_BETA_MAIN,
            RUST_BETA_LIB,
        ];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("rust-crate-roots-fresh", &fresh)
        );
        assert!(
            rust_use_links(&db, RUST_BETA_LIB.0),
            "{:#?}",
            edge_projection(&db)
        );
        assert_eq!(
            lane_marker(&db, crate::rust_crate_roots::VERSION_KEY),
            version
        );
    }

    /// W7-19h: an index without the Rust crate roots marker (built before
    /// it, or a sync that stopped) has every Rust file re-resolved by its
    /// next sync, with nothing changed: the crate root edge an older sync
    /// lost comes back.
    #[test]
    fn an_index_without_the_rust_crate_roots_marker_re_resolves_every_rust_file() {
        let version = Some(crate::rust_crate_roots::VERSION.to_string());
        let dir = TestDir::new("rust-crate-roots-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            RUST_WORKSPACE,
            RUST_ALPHA_MANIFEST,
            RUST_ALPHA,
            RUST_BETA_MANIFEST,
            RUST_BETA_LIB,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            lane_marker(&db, crate::rust_crate_roots::VERSION_KEY),
            version
        );
        assert!(
            rust_use_links(&db, RUST_BETA_LIB.0),
            "{:#?}",
            edge_projection(&db)
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'imports' \
                     AND source = 'file:crates/alpha/src/lib.rs' \
                     AND target = 'file:crates/beta/src/lib.rs'",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the crate root edge an older sync lost");
            store
                .delete_project_metadata(crate::rust_crate_roots::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            lane_marker(&db, crate::rust_crate_roots::VERSION_KEY),
            version
        );
        assert_eq!(edge_projection(&db), expected);
    }

    const RN_PACKAGE: (&str, &str) = (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"react-native\":\"^0.74.0\"}}\n",
    );
    const RN_SWIFT: (&str, &str) = (
        "ios/CaptureView.swift",
        "import Foundation\n\n@objc(CaptureView)\nclass CaptureView: NSObject {\n  @objc func syncSettings() {}\n}\n",
    );
    const RN_SHIM_EMPTY: (&str, &str) = (
        "ios/CaptureView.m",
        "#import <React/RCTBridgeModule.h>\n\n@interface RCT_EXTERN_MODULE(CaptureView, NSObject)\n@end\n",
    );
    const RN_SHIM: (&str, &str) = (
        "ios/CaptureView.m",
        "#import <React/RCTBridgeModule.h>\n\n@interface RCT_EXTERN_MODULE(CaptureView, NSObject)\nRCT_EXTERN_METHOD(syncSettings)\n@end\n",
    );
    const RN_SAVE: (&str, &str) = (
        "src/save.ts",
        "import { NativeModules } from 'react-native';\n\nexport function save() {\n  NativeModules.CaptureView.syncSettings();\n}\n",
    );

    /// Whether the JS call is the bridge's edge to the Swift method the shim
    /// exports.
    fn rn_bridged(db: &Path) -> bool {
        edge_projection(db).iter().any(|edge| {
            edge.starts_with(
                "src/save.ts:save -[calls]-> ios/CaptureView.swift:CaptureView::syncSettings ",
            ) && edge.contains("\"framework\":\"react-native-bridge\"")
        })
    }

    fn rn_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(
                codegraph_resolve::frameworks::react_native::ROW_KEY_PREFIX,
            )
            .unwrap()
    }

    /// W7-21: a sync failing after it gave the shim its method, before
    /// resolving, leaves the bridge marker away, and the next sync, with
    /// nothing changed, links the JS call to the Swift method and writes the
    /// shim's new row, as a fresh index has them. (The other lanes' markers
    /// the failure withdrew re-resolve their own files too, so the marker and
    /// the rows are what this test reads.)
    #[test]
    fn a_bridge_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let version = Some(crate::rn_bridges::VERSION.to_string());
        let dir = TestDir::new("rn-bridges-fail");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RN_PACKAGE, RN_SWIFT, RN_SHIM_EMPTY, RN_SAVE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lane_marker(&db, crate::rn_bridges::VERSION_KEY), version);
        assert!(!rn_bridged(&db), "{:#?}", edge_projection(&db));
        let empty_rows = rn_rows(&db);

        write_files(dir.path(), &[RN_SHIM]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [RN_SHIM.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(
            lane_marker(&db, crate::rn_bridges::VERSION_KEY),
            None,
            "a failed sync leaves the marker away"
        );
        assert_eq!(rn_rows(&db), empty_rows, "rows are written last");
        assert!(!rn_bridged(&db), "{:#?}", edge_projection(&db));

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = [RN_PACKAGE, RN_SWIFT, RN_SHIM, RN_SAVE];
        assert_eq!(
            edge_projection(&db),
            fresh_edges("rn-bridges-fresh", &fresh)
        );
        assert!(rn_bridged(&db), "{:#?}", edge_projection(&db));
        assert_ne!(rn_rows(&db), empty_rows);
        let fresh_dir = TestDir::new("rn-bridges-fresh-rows");
        write_files(fresh_dir.path(), &fresh);
        let fresh_db = default_db_path(fresh_dir.path()).unwrap();
        sync_changed_paths(
            fresh_dir.path(),
            &fresh_db,
            fresh.iter().map(|(path, _)| *path),
        )
        .unwrap();
        assert_eq!(rn_rows(&db), rn_rows(&fresh_db));
        assert_eq!(lane_marker(&db, crate::rn_bridges::VERSION_KEY), version);
    }

    const RN_CACHE: (&str, &str) = (
        "ios/Cache.m",
        "#import <Foundation/Foundation.h>\n\n@implementation Cache\n- (id)fetchEntryForKey:(NSString *)key {\n  return nil;\n}\n- (void)run:(CaptureView *)view {\n  [view syncSettingsWithMode:1];\n}\n@end\n",
    );
    const RN_CALLER: (&str, &str) = (
        "ios/Caller.swift",
        "import Foundation\n\nfunc load(cache: Cache) {\n  _ = cache.fetchEntry(forKey: \"x\")\n}\n",
    );

    /// W7-21: an index without the bridge marker (built before it, or a sync
    /// that stopped) rebuilds every bridge row, a row of no file's facts
    /// dropped, and re-resolves the JS, Swift and Objective-C files of a
    /// project that shows a bridge, with nothing changed: the bridge edges
    /// an older sync lost, from each of the three languages, come back. The
    /// other lanes' markers stay, so none of them re-resolves those files.
    #[test]
    fn an_index_without_the_bridge_marker_rebuilds_its_rows_and_re_resolves_the_bridged_files() {
        let version = Some(crate::rn_bridges::VERSION.to_string());
        let dir = TestDir::new("rn-bridges-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RN_PACKAGE, RN_SWIFT, RN_SHIM, RN_SAVE, RN_CACHE, RN_CALLER];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(lane_marker(&db, crate::rn_bridges::VERSION_KEY), version);
        assert!(rn_bridged(&db), "{:#?}", edge_projection(&db));
        let expected = edge_projection(&db);
        let bridged = |edges: &[String], from: &str| {
            edges.iter().any(|edge| {
                edge.starts_with(from) && edge.contains("\"framework\":\"swift-objc-bridge\"")
            })
        };
        assert!(
            bridged(
                &expected,
                "ios/Caller.swift:load -[calls]-> ios/Cache.m:Cache::fetchEntryForKey: "
            ),
            "{expected:#?}"
        );
        assert!(
            bridged(
                &expected,
                "ios/Cache.m:Cache::run: -[calls]-> ios/CaptureView.swift:CaptureView::syncSettings "
            ),
            "{expected:#?}"
        );
        let rows = rn_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key.ends_with("ios/CaptureView.m")),
            "{rows:#?}"
        );
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND json_extract(metadata, \
                     '$.resolvedBy') = 'framework' AND source IN (SELECT id FROM nodes WHERE \
                     file_path IN ('src/save.ts', 'ios/Caller.swift', 'ios/Cache.m'))",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 3, "the bridge edges an older sync lost");
            for (key, _) in &rows {
                store.delete_project_metadata(key).unwrap();
            }
            store
                .set_project_metadata(
                    &codegraph_resolve::frameworks::react_native::row_key("src/gone.ts"),
                    "{\"aliases\":[[\"gone\",\"Gone\"]]}",
                )
                .unwrap();
            store
                .delete_project_metadata(crate::rn_bridges::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(rn_rows(&db).len(), 1, "only the stale row is left");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(lane_marker(&db, crate::rn_bridges::VERSION_KEY), version);
        assert_eq!(rn_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    const RN_CAM_EXPORT: (&str, &str) = (
        "ios/Cam.m",
        "#import <React/RCTBridgeModule.h>\n\n@implementation Cam\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(snap:(id)options) {}\n@end\n",
    );
    const RN_SHOOT: (&str, &str) = (
        "src/shoot.ts",
        "import { NativeModules } from 'react-native';\n\nexport function shoot() {\n  NativeModules.Cam.snap();\n}\n",
    );

    /// W7-21: a bootstrap re-resolves a project that shows one bridge only,
    /// the Swift–Objective-C one, or the React Native one by its macro with
    /// no `package.json` naming React Native (so no Fabric beside it), as
    /// it does one that shows them all.
    #[test]
    fn a_bootstrap_re_resolves_a_project_that_shows_one_bridge() {
        let version = Some(crate::rn_bridges::VERSION.to_string());
        for (tag, files, from) in [
            (
                "rn-bridges-bootstrap-swift-objc",
                &[RN_CACHE, RN_CALLER][..],
                "ios/Caller.swift:load -[calls]-> ",
            ),
            (
                "rn-bridges-bootstrap-macro",
                &[RN_CAM_EXPORT, RN_SHOOT][..],
                "src/shoot.ts:shoot -[calls]-> ",
            ),
        ] {
            let dir = TestDir::new(tag);
            let db = default_db_path(dir.path()).unwrap();
            write_files(dir.path(), files);
            sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
            let expected = edge_projection(&db);
            assert!(
                expected.iter().any(|edge| {
                    edge.starts_with(from) && edge.contains("\"resolvedBy\":\"framework\"")
                }),
                "{tag}: {expected:#?}"
            );
            {
                let store = Store::open(&db).unwrap();
                let dropped = store
                    .connection()
                    .execute(
                        "DELETE FROM edges WHERE kind = 'calls' AND json_extract(metadata, \
                         '$.resolvedBy') = 'framework'",
                        [],
                    )
                    .unwrap();
                assert_eq!(dropped, 1, "{tag}: the bridge edge an older sync lost");
                store
                    .delete_project_metadata(crate::rn_bridges::VERSION_KEY)
                    .unwrap();
            }
            assert_ne!(edge_projection(&db), expected, "{tag}");
            sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
            assert_eq!(
                lane_marker(&db, crate::rn_bridges::VERSION_KEY),
                version,
                "{tag}"
            );
            assert_eq!(edge_projection(&db), expected, "{tag}");
        }
    }

    /// W7-21: a sync reads an unchanged file's bridge facts from its row,
    /// not its text: with the shim's row naming another module, the JS call
    /// a sync re-resolves finds no `CaptureView` method, until the shim
    /// itself changes and is read again.
    #[test]
    fn a_sync_reads_an_unchanged_files_bridge_facts_from_its_row() {
        let dir = TestDir::new("rn-bridges-trusted");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [RN_PACKAGE, RN_SWIFT, RN_SHIM, RN_SAVE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert!(rn_bridged(&db), "{:#?}", edge_projection(&db));
        {
            let elsewhere = RN_SHIM.1.replace(
                "RCT_EXTERN_MODULE(CaptureView",
                "RCT_EXTERN_MODULE(Elsewhere",
            );
            let facts = codegraph_resolve::frameworks::react_native::BridgeFacts::of_text(
                RN_SHIM.0, &elsewhere,
            );
            assert_eq!(facts.externs[0].module, "Elsewhere");
            Store::open(&db)
                .unwrap()
                .set_project_metadata(
                    &codegraph_resolve::frameworks::react_native::row_key(RN_SHIM.0),
                    &facts.encode(),
                )
                .unwrap();
        }
        let save = format!("{}\nexport function spare() {{}}\n", RN_SAVE.1);
        write_files(dir.path(), &[(RN_SAVE.0, save.as_str())]);
        sync_changed_paths(dir.path(), &db, [RN_SAVE.0]).unwrap();
        assert!(!rn_bridged(&db), "{:#?}", edge_projection(&db));
        let shim = format!("{}\n", RN_SHIM.1);
        write_files(dir.path(), &[(RN_SHIM.0, shim.as_str())]);
        sync_changed_paths(dir.path(), &db, [RN_SHIM.0]).unwrap();
        assert!(rn_bridged(&db), "{:#?}", edge_projection(&db));
    }

    fn dart_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::dart_heads::VERSION_KEY)
            .unwrap()
    }

    fn dart_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::dart_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    const DART_BASE: (&str, &str) = ("lib/base.dart", "class Base {\n  void emit(int x) {}\n}\n");
    const DART_CUBIT: (&str, &str) = ("lib/cubit.dart", "class Cubit {}\n");
    const DART_CUBIT_EXTENDING: (&str, &str) = ("lib/cubit.dart", "class Cubit extends Base {}\n");
    const DART_SUB: (&str, &str) = (
        "lib/sub.dart",
        "class Sub extends Cubit {\n  void run() {\n    emit(1);\n  }\n}\n",
    );
    const DART_SPARE: (&str, &str) = ("lib/spare.dart", "class Spare {}\n");
    const DART_SPARE_EDITED: (&str, &str) = ("lib/spare.dart", "class Spare {}\n\nclass Idle {}\n");

    /// W4-DART-01: a sync failing after it wrote `class Cubit extends Base`,
    /// before resolving, leaves the next sync, with nothing changed or with
    /// an unrelated change, to bring the bare `emit(1)` in a subclass in
    /// another file to `Base::emit`, as a fresh index has it.
    #[test]
    fn a_dart_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        let to_emit = |db: &Path| {
            edge_projection(db)
                .iter()
                .any(|edge| edge.contains("-[calls]-> lib/base.dart:Base::emit"))
        };
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "dart-heads-fail-unrelated"
            } else {
                "dart-heads-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [DART_BASE, DART_CUBIT, DART_SUB, DART_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                dart_marker(&db),
                Some(crate::dart_heads::VERSION.to_string())
            );
            assert!(!to_emit(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[DART_CUBIT_EXTENDING]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [DART_CUBIT.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                dart_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[DART_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [DART_SPARE.0]).unwrap();
                DART_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                DART_SPARE
            };
            assert_eq!(
                dart_marker(&db),
                Some(crate::dart_heads::VERSION.to_string())
            );
            let fresh = [DART_BASE, DART_CUBIT_EXTENDING, DART_SUB, spare];
            let name = if unrelated {
                "dart-heads-fresh-unrelated"
            } else {
                "dart-heads-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(to_emit(&db), "{:#?}", edge_projection(&db));
        }
    }

    /// W4-DART-01: an index whose Dart marker and rows alone are gone has
    /// every Dart file re-resolved and every row rebuilt by its next sync,
    /// even one with nothing changed: the call an older sync lost comes back.
    #[test]
    fn an_index_without_only_the_dart_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::dart_heads::VERSION.to_string());
        let dir = TestDir::new("dart-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DART_BASE, DART_CUBIT_EXTENDING, DART_SUB, DART_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(dart_marker(&db), version);
        let rows = dart_rows(&db);
        assert_eq!(rows.len(), 4, "{rows:#?}");
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE qualified_name = 'Base::emit')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::dart_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::dart_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            lua_marker(&db),
            Some(crate::lua_modules::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(dart_marker(&db), version);
        assert_eq!(dart_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-DART-01: a marker still current over a row that cannot be read
    /// rebuilds the rows and re-resolves every Dart file in a sync with
    /// nothing changed; a rebuild stopping once the old rows are purged and
    /// one is written back leaves the marker away, so the next sync finishes
    /// it.
    #[test]
    fn a_dart_row_that_cannot_be_read_is_rebuilt_even_after_a_rebuild_stops_partway() {
        let version = Some(crate::dart_heads::VERSION.to_string());
        let dir = TestDir::new("dart-heads-unreadable");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DART_BASE, DART_CUBIT_EXTENDING, DART_SUB, DART_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let rows = dart_rows(&db);
        let expected = edge_projection(&db);
        Store::open(&db)
            .unwrap()
            .set_project_metadata(
                &codegraph_resolve::dart_scope::head_row_key(DART_SUB.0),
                "{not a row",
            )
            .unwrap();
        FAIL_AFTER_DART_ROW_PURGE.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_AFTER_DART_ROW_PURGE.with(|fail| fail.set(false));
        assert!(
            failed.is_err(),
            "the injected failure must stop the rebuild"
        );
        assert_eq!(
            dart_marker(&db),
            None,
            "the rebuild withdrew the marker first"
        );
        assert_eq!(dart_rows(&db).len(), 1, "{:#?}", dart_rows(&db));
        let dropped = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                 WHERE qualified_name = 'Base::emit')",
                [],
            )
            .unwrap();
        assert_eq!(dropped, 1, "the call edge a later loss takes");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(dart_marker(&db), version);
        assert_eq!(dart_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    fn dart_library_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::dart_libraries::VERSION_KEY)
            .unwrap()
    }

    fn dart_library_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::dart_libraries::LIBRARY_ROW_KEY_PREFIX)
            .unwrap()
    }

    const DART_PUBSPEC: (&str, &str) = ("pubspec.yaml", "name: app\n");
    const DART_UTIL: (&str, &str) = ("lib/util.dart", "int helper() => 1;\n");
    const DART_LIB: (&str, &str) = ("lib/lib.dart", "part 'piece.dart';\n");
    const DART_LIB_IMPORTING: (&str, &str) = (
        "lib/lib.dart",
        "import 'util.dart';\n\npart 'piece.dart';\n",
    );
    const DART_PIECE: (&str, &str) = (
        "lib/piece.dart",
        "part of 'lib.dart';\n\nint run() => helper();\n",
    );

    fn reaches_helper(db: &Path) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains("-[calls]-> lib/util.dart:helper"))
    }

    /// W4-DART-02: a sync failing after it wrote the library's new import,
    /// before resolving, leaves the next sync, with nothing changed or with
    /// an unrelated change, to bring the unchanged part's `helper()` to what
    /// the import brings in, as a fresh index has it.
    #[test]
    fn a_dart_library_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "dart-libraries-fail-unrelated"
            } else {
                "dart-libraries-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [DART_UTIL, DART_LIB, DART_PIECE, DART_SPARE];
            write_files(dir.path(), &[DART_PUBSPEC]);
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert_eq!(
                dart_library_marker(&db),
                Some(crate::dart_libraries::VERSION.to_string())
            );
            assert!(!reaches_helper(&db), "{:#?}", edge_projection(&db));

            write_files(dir.path(), &[DART_LIB_IMPORTING]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [DART_LIB.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                dart_library_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[DART_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [DART_SPARE.0]).unwrap();
                DART_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                DART_SPARE
            };
            assert_eq!(
                dart_library_marker(&db),
                Some(crate::dart_libraries::VERSION.to_string())
            );
            let fresh = [
                DART_PUBSPEC,
                DART_UTIL,
                DART_LIB_IMPORTING,
                DART_PIECE,
                spare,
            ];
            let name = if unrelated {
                "dart-libraries-fresh-unrelated"
            } else {
                "dart-libraries-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(reaches_helper(&db), "{:#?}", edge_projection(&db));
        }
    }

    fn dart_pubspec_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::dart_libraries::PUBSPEC_ROW_KEY_PREFIX)
            .unwrap()
    }

    const KIT_PUBSPEC: (&str, &str) = ("packages/kit/pubspec.yaml", "name: kit\n");
    const KIT_PUBSPEC_RENAMED: (&str, &str) = ("packages/kit/pubspec.yaml", "name: kit2\n");
    const KIT_LIB: (&str, &str) = ("packages/kit/lib/kit.dart", "int shown() => 1;\n");
    const KIT_MAIN: (&str, &str) = (
        "lib/main.dart",
        "import 'package:kit/kit.dart';\n\nint run() => shown();\n",
    );

    fn imports_kit(db: &Path) -> bool {
        edge_projection(db)
            .iter()
            .any(|edge| edge.contains("lib/main.dart -[imports]-> packages/kit/lib/kit.dart"))
    }

    /// W5-06: a sync failing after a package's pubspec renamed it, before
    /// resolving, leaves the next sync, with nothing changed or with an
    /// unrelated change, to strand the import of the old name as a fresh
    /// index does, and to write the pubspec rows a fresh index writes.
    #[test]
    fn a_pubspec_sync_failing_after_its_mutations_leaves_the_next_one_to_reconcile() {
        for unrelated in [false, true] {
            let dir = TestDir::new(if unrelated {
                "dart-pubspec-fail-unrelated"
            } else {
                "dart-pubspec-fail"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [DART_PUBSPEC, KIT_PUBSPEC, KIT_LIB, KIT_MAIN, DART_SPARE];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            assert!(imports_kit(&db), "{:#?}", edge_projection(&db));
            assert_eq!(
                dart_pubspec_rows(&db).len(),
                2,
                "{:#?}",
                dart_pubspec_rows(&db)
            );

            write_files(dir.path(), &[KIT_PUBSPEC_RENAMED]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, [KIT_PUBSPEC.0]);
            FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                dart_library_marker(&db),
                None,
                "a failed sync leaves the marker away"
            );

            let spare = if unrelated {
                write_files(dir.path(), &[DART_SPARE_EDITED]);
                sync_changed_paths(dir.path(), &db, [DART_SPARE.0]).unwrap();
                DART_SPARE_EDITED
            } else {
                sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
                DART_SPARE
            };
            assert_eq!(
                dart_library_marker(&db),
                Some(crate::dart_libraries::VERSION.to_string())
            );
            let fresh = [DART_PUBSPEC, KIT_PUBSPEC_RENAMED, KIT_LIB, KIT_MAIN, spare];
            let name = if unrelated {
                "dart-pubspec-fresh-unrelated"
            } else {
                "dart-pubspec-fresh"
            };
            assert_eq!(edge_projection(&db), fresh_edges(name, &fresh));
            assert!(!imports_kit(&db), "{:#?}", edge_projection(&db));
            let rows = dart_pubspec_rows(&db);
            assert!(
                rows.iter().any(
                    |(key, value)| key == "dart_pubspec:packages/kit" && value.contains("kit2")
                ),
                "{rows:#?}"
            );
        }
    }

    /// W5-06: an index at the Dart library marker's version 1, which wrote
    /// no pubspec rows, or with a pubspec row that cannot be read, has every
    /// Dart file re-resolved and both kinds of row rebuilt by its next sync,
    /// even one with nothing changed: the import edge an older sync lost
    /// comes back and the rows are a fresh index's.
    #[test]
    fn an_index_at_marker_one_or_with_a_corrupt_pubspec_row_rebuilds_the_rows() {
        for corrupt in [false, true] {
            let dir = TestDir::new(if corrupt {
                "dart-pubspec-corrupt-row"
            } else {
                "dart-pubspec-marker-one"
            });
            let db = default_db_path(dir.path()).unwrap();
            let initial = [DART_PUBSPEC, KIT_PUBSPEC, KIT_LIB, KIT_MAIN];
            write_files(dir.path(), &initial);
            sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
            let expected = edge_projection(&db);
            let rows = dart_pubspec_rows(&db);
            assert_eq!(rows.len(), 2, "{rows:#?}");
            {
                let store = Store::open(&db).unwrap();
                let dropped = store
                    .connection()
                    .execute(
                        "DELETE FROM edges WHERE kind = 'imports' AND target = \
                         (SELECT id FROM nodes WHERE kind = 'file' \
                         AND file_path = 'packages/kit/lib/kit.dart')",
                        [],
                    )
                    .unwrap();
                assert_eq!(dropped, 1, "the import edge an older sync lost");
                if corrupt {
                    store
                        .set_project_metadata("dart_pubspec:packages/kit", "not json")
                        .unwrap();
                } else {
                    store
                        .delete_project_metadata_with_prefix(
                            codegraph_resolve::dart_libraries::PUBSPEC_ROW_KEY_PREFIX,
                        )
                        .unwrap();
                    store
                        .set_project_metadata(crate::dart_libraries::VERSION_KEY, "1")
                        .unwrap();
                }
            }
            sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
            assert_eq!(edge_projection(&db), expected);
            assert_eq!(dart_pubspec_rows(&db), rows);
            assert_eq!(
                dart_library_marker(&db),
                Some(crate::dart_libraries::VERSION.to_string())
            );
        }
    }

    /// W5-06: a pubspec the sync is not told of is not read, and one in a
    /// folder above no Dart file is no input: neither moves anything.
    #[test]
    fn a_pubspec_is_read_only_when_a_sync_is_told_of_it() {
        let dir = TestDir::new("dart-pubspec-untold");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DART_PUBSPEC, KIT_PUBSPEC, KIT_LIB, KIT_MAIN, DART_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let rows = dart_pubspec_rows(&db);
        write_files(
            dir.path(),
            &[KIT_PUBSPEC_RENAMED, ("tools/pubspec.yaml", "name: tools\n")],
        );
        write_files(dir.path(), &[DART_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, [DART_SPARE.0]).unwrap();
        assert!(imports_kit(&db), "an untold rename is not read yet");
        assert_eq!(dart_pubspec_rows(&db), rows);
        sync_changed_paths(dir.path(), &db, ["tools/pubspec.yaml"]).unwrap();
        assert!(!imports_kit(&db), "{:#?}", edge_projection(&db));
        let rows = dart_pubspec_rows(&db);
        assert!(
            !rows.iter().any(|(key, _)| key == "dart_pubspec:tools"),
            "{rows:#?}"
        );
    }

    /// W4-DART-02: an index whose Dart library marker and rows alone are
    /// gone has every Dart file re-resolved and every row rebuilt by its next
    /// sync, even one with nothing changed: the call an older sync lost comes
    /// back.
    #[test]
    fn an_index_without_only_the_dart_library_marker_rebuilds_its_rows_with_nothing_changed() {
        let version = Some(crate::dart_libraries::VERSION.to_string());
        let dir = TestDir::new("dart-libraries-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DART_UTIL, DART_LIB_IMPORTING, DART_PIECE, DART_SPARE];
        write_files(dir.path(), &[DART_PUBSPEC]);
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(dart_library_marker(&db), version);
        let rows = dart_library_rows(&db);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        let expected = edge_projection(&db);
        assert!(reaches_helper(&db), "{expected:#?}");
        {
            let store = Store::open(&db).unwrap();
            let dropped = store
                .connection()
                .execute(
                    "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                     WHERE file_path = 'lib/util.dart' AND name = 'helper')",
                    [],
                )
                .unwrap();
            assert_eq!(dropped, 1, "the call edge an older sync lost");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::dart_libraries::LIBRARY_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::dart_libraries::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        assert_eq!(
            dart_marker(&db),
            Some(crate::dart_heads::VERSION.to_string()),
            "the other markers stay"
        );
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(dart_library_marker(&db), version);
        assert_eq!(dart_library_rows(&db), rows);
        assert_eq!(edge_projection(&db), expected);
    }

    /// W4-DART-02: a marker still current over a row that cannot be read
    /// rebuilds the rows and re-resolves every Dart file in a sync with
    /// nothing changed; a rebuild stopping once the old rows are purged and
    /// one is written back leaves the marker away, so the next sync finishes
    /// it.
    #[test]
    fn a_dart_library_row_that_cannot_be_read_is_rebuilt_even_after_a_rebuild_stops_partway() {
        let version = Some(crate::dart_libraries::VERSION.to_string());
        let dir = TestDir::new("dart-libraries-unreadable");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [DART_UTIL, DART_LIB_IMPORTING, DART_PIECE, DART_SPARE];
        write_files(dir.path(), &[DART_PUBSPEC]);
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let rows = dart_library_rows(&db);
        let expected = edge_projection(&db);
        Store::open(&db)
            .unwrap()
            .set_project_metadata(
                &codegraph_resolve::dart_libraries::library_row_key(DART_PIECE.0),
                "{not a row",
            )
            .unwrap();
        FAIL_AFTER_DART_LIBRARY_ROW_PURGE.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_AFTER_DART_LIBRARY_ROW_PURGE.with(|fail| fail.set(false));
        assert!(
            failed.is_err(),
            "the injected failure must stop the rebuild"
        );
        assert_eq!(
            dart_library_marker(&db),
            None,
            "the rebuild withdrew the marker first"
        );
        assert_eq!(
            dart_library_rows(&db).len(),
            1,
            "{:#?}",
            dart_library_rows(&db)
        );
        let dropped = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "DELETE FROM edges WHERE kind = 'calls' AND target = (SELECT id FROM nodes \
                 WHERE file_path = 'lib/util.dart' AND name = 'helper')",
                [],
            )
            .unwrap();
        assert_eq!(dropped, 1, "the call edge a later loss takes");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(dart_library_marker(&db), version);
        assert_eq!(dart_library_rows(&db), rows);
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

    const SOL_CONTEXT: (&str, &str) = (
        "contracts/utils/Context.sol",
        "pragma solidity ^0.8.20;\n\nabstract contract Context {\n    function _msgSender() internal view virtual returns (address) {\n        return msg.sender;\n    }\n}\n",
    );
    const SOL_OVERRIDE: (&str, &str) = (
        "contracts/metatx/ERC2771Context.sol",
        "pragma solidity ^0.8.20;\n\nimport {Context} from \"../utils/Context.sol\";\n\nabstract contract ERC2771Context is Context {\n    function _msgSender() internal view virtual override returns (address) {\n        return msg.sender;\n    }\n}\n",
    );
    const SOL_MIDDLE: (&str, &str) = (
        "contracts/Middle.sol",
        "pragma solidity ^0.8.20;\n\nimport \"./utils/Context.sol\";\n\nabstract contract Middle is Context {}\n",
    );
    const SOL_GOVERNOR: (&str, &str) = (
        "contracts/governance/Governor.sol",
        "pragma solidity ^0.8.20;\n\nimport \"../Middle.sol\";\n\nabstract contract Governor is Middle {\n    function castVote() public returns (address) {\n        return _msgSender();\n    }\n}\n",
    );
    const SOL_SPARE: (&str, &str) = (
        "contracts/Spare.sol",
        "pragma solidity ^0.8.20;\n\ncontract Spare {}\n",
    );
    const SOL_SPARE_EDITED: (&str, &str) = (
        "contracts/Spare.sol",
        "pragma solidity ^0.8.20;\n\ncontract Spare {\n    uint256 internal count;\n}\n",
    );

    fn solidity_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(codegraph_resolve::solidity_scope::HEAD_ROW_KEY_PREFIX)
            .unwrap()
    }

    /// W4-SOL-01: an index without the Solidity marker (built before it, or
    /// left by a failed sync) has every Solidity file re-resolved by its next
    /// sync and its head rows rebuilt: a bare call an older index linked
    /// wrongly from an unchanged contract ends where a fresh index puts it.
    #[test]
    fn an_index_without_the_solidity_marker_rebuilds_the_head_rows() {
        let version = Some(crate::solidity_heads::VERSION.to_string());
        let dir = TestDir::new("solidity-heads-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            SOL_CONTEXT,
            SOL_OVERRIDE,
            SOL_MIDDLE,
            SOL_GOVERNOR,
            SOL_SPARE,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let marker = || {
            Store::open(&db)
                .unwrap()
                .get_project_metadata(crate::solidity_heads::VERSION_KEY)
                .unwrap()
        };
        assert_eq!(marker(), version);
        let rows = solidity_rows(&db);
        assert!(
            rows.iter()
                .any(|(key, _)| key == "solidity_heads:contracts/Middle.sol"),
            "{rows:#?}"
        );
        let expected = edge_projection(&db);
        {
            let store = Store::open(&db).unwrap();
            let moved = store
                .connection()
                .execute(
                    "UPDATE edges SET target = (SELECT id FROM nodes WHERE qualified_name = \
                     'ERC2771Context::_msgSender') WHERE kind = 'calls' AND target = (SELECT id \
                     FROM nodes WHERE qualified_name = 'Context::_msgSender')",
                    [],
                )
                .unwrap();
            assert_eq!(moved, 1, "the call edge an older index linked elsewhere");
            store
                .delete_project_metadata_with_prefix(
                    codegraph_resolve::solidity_scope::HEAD_ROW_KEY_PREFIX,
                )
                .unwrap();
            store
                .delete_project_metadata(crate::solidity_heads::VERSION_KEY)
                .unwrap();
        }
        assert_ne!(edge_projection(&db), expected);
        write_files(dir.path(), &[SOL_SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, ["contracts/Spare.sol"]).unwrap();
        assert_eq!(marker(), version);
        let mut rebuilt = solidity_rows(&db);
        let mut wanted = rows;
        // The edited file's row is its new one.
        rebuilt.retain(|(key, _)| key != "solidity_heads:contracts/Spare.sol");
        wanted.retain(|(key, _)| key != "solidity_heads:contracts/Spare.sol");
        assert_eq!(rebuilt, wanted);
        assert_eq!(edge_projection(&db), expected);
    }

    const LIQ_LAYOUT: (&str, &str) = (
        "layout/theme.liquid",
        "{% section 'main-product' %}{{ content_for_layout }}\n",
    );
    const LIQ_SECTION: (&str, &str) = (
        "sections/main-product.liquid",
        "<div>{{ product.title }}</div>\n",
    );
    const LIQ_TEMPLATE: (&str, &str) = (
        "templates/product.json",
        "{\"sections\":{\"main\":{\"type\":\"main-product\"}},\"order\":[\"main\"]}",
    );
    const LIQ_SPARE: (&str, &str) = ("src/spare.ts", "export const spare = 1;\n");

    fn stored_paths(db: &Path) -> Vec<String> {
        Store::open(db)
            .unwrap()
            .all_files()
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect()
    }

    /// W4-LIQ-01: an index without the Shopify theme marker (left by a
    /// failed sync, which withdraws it first) has its next sync hand every
    /// indexed JSON template to the per-path pass: one that no theme holds
    /// any more leaves though the row never recorded its theme, and the row
    /// and the marker are written again.
    #[test]
    fn an_index_without_the_shopify_marker_rechecks_every_json_template() {
        let version = Some(crate::shopify_themes::VERSION.to_string());
        let dir = TestDir::new("shopify-themes-bootstrap");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [LIQ_LAYOUT, LIQ_SECTION, LIQ_TEMPLATE, LIQ_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let metadata = |key: &str| Store::open(&db).unwrap().get_project_metadata(key).unwrap();
        assert_eq!(metadata(crate::shopify_themes::VERSION_KEY), version);
        assert_eq!(
            metadata(crate::shopify_themes::ROW_KEY).as_deref(),
            Some("[\"\"]")
        );
        assert!(
            stored_paths(&db).contains(&LIQ_TEMPLATE.0.to_string()),
            "{:?}",
            stored_paths(&db)
        );
        {
            let store = Store::open(&db).unwrap();
            store
                .delete_project_metadata(crate::shopify_themes::ROW_KEY)
                .unwrap();
            store
                .delete_project_metadata(crate::shopify_themes::VERSION_KEY)
                .unwrap();
        }
        fs::remove_file(dir.path().join(LIQ_LAYOUT.0)).unwrap();
        sync_changed_paths(dir.path(), &db, [LIQ_LAYOUT.0]).unwrap();
        assert_eq!(metadata(crate::shopify_themes::VERSION_KEY), version);
        assert_eq!(
            metadata(crate::shopify_themes::ROW_KEY).as_deref(),
            Some("[]")
        );
        assert_eq!(
            stored_paths(&db),
            [LIQ_SECTION.0, LIQ_SPARE.0],
            "a JSON template no theme holds leaves"
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "shopify-themes-bootstrap-fresh",
                &[LIQ_SECTION, LIQ_TEMPLATE, LIQ_SPARE]
            )
        );
    }

    /// W4-LIQ-01: a sync that stops after the theme lane found a theme come
    /// or go, before the per-path pass stored or dropped any of its JSON,
    /// leaves the moved roots recorded: the next sync with nothing changed,
    /// or with an unrelated change, takes them up, though no indexed file
    /// points at the theme (one of JSON templates only), with the marker in
    /// place or withdrawn by the stopped sync's first change.
    #[test]
    fn a_sync_stopped_before_a_shopify_themes_json_moved_leaves_it_to_the_next() {
        const TEMPLATE: (&str, &str) = (
            "shop/templates/index.json",
            "{\"sections\":{\"main\":{\"type\":\"main\"}},\"order\":[\"main\"]}",
        );
        const SCHEMA: (&str, &str) = ("shop/config/settings_schema.json", "[]");
        const SPARE_EDITED: (&str, &str) = ("src/spare.ts", "export const spare = 2;\n");
        let version = Some(crate::shopify_themes::VERSION.to_string());
        let dir = TestDir::new("shopify-themes-pending");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [TEMPLATE, LIQ_SPARE];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            stored_paths(&db),
            [LIQ_SPARE.0],
            "no theme holds the JSON yet"
        );
        let metadata = |key: &str| Store::open(&db).unwrap().get_project_metadata(key).unwrap();
        let fail_once = |paths: &[&str]| {
            FAIL_AFTER_SHOPIFY_REFRESH.with(|fail| fail.set(true));
            let failed = sync_changed_paths(dir.path(), &db, paths.iter().copied());
            FAIL_AFTER_SHOPIFY_REFRESH.with(|fail| fail.set(false));
            assert!(failed.is_err(), "the injected failure must stop the sync");
            assert_eq!(
                metadata(crate::shopify_themes::PENDING_KEY).as_deref(),
                Some("[\"shop\"]")
            );
        };

        // The schema comes and the sync stops; one with nothing changed
        // takes the theme's JSON in.
        write_files(dir.path(), &[SCHEMA]);
        fail_once(&[SCHEMA.0]);
        assert_eq!(stored_paths(&db), [LIQ_SPARE.0]);
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(stored_paths(&db), [TEMPLATE.0, LIQ_SPARE.0]);
        assert_eq!(metadata(crate::shopify_themes::PENDING_KEY), None);
        assert_eq!(
            metadata(crate::shopify_themes::ROW_KEY).as_deref(),
            Some("[\"shop\"]")
        );

        // The schema goes and the sync stops; an unrelated change takes the
        // JSON out.
        fs::remove_file(dir.path().join(SCHEMA.0)).unwrap();
        fail_once(&[SCHEMA.0]);
        write_files(dir.path(), &[SPARE_EDITED]);
        sync_changed_paths(dir.path(), &db, [LIQ_SPARE.0]).unwrap();
        assert_eq!(stored_paths(&db), [LIQ_SPARE.0]);
        assert_eq!(metadata(crate::shopify_themes::PENDING_KEY), None);

        // It comes again and the sync stops with the marker withdrawn, as its
        // first change would; one with nothing changed takes the JSON in.
        write_files(dir.path(), &[SCHEMA]);
        fail_once(&[SCHEMA.0]);
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(crate::shopify_themes::VERSION_KEY)
            .unwrap();
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(stored_paths(&db), [TEMPLATE.0, LIQ_SPARE.0]);
        assert_eq!(metadata(crate::shopify_themes::VERSION_KEY), version);
        assert_eq!(metadata(crate::shopify_themes::PENDING_KEY), None);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "shopify-themes-pending-fresh",
                &[TEMPLATE, SCHEMA, SPARE_EDITED]
            )
        );
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

    /// W5-09d: the outcome carries each changed path's row as the sync left
    /// it, the same row a later read-only open finds, and `None` for a file
    /// the sync removed.
    #[test]
    fn changed_records_are_the_rows_the_sync_left() {
        let dir = TestDir::new("watch-changed-records");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 42; }\n",
        )
        .unwrap();
        fs::write(dir.path().join("src/gone.ts"), "export const gone = 1;\n").unwrap();
        let db = default_db_path(dir.path()).unwrap();
        let _ = sync_changed_paths(dir.path(), &db, ["src/app.ts", "src/gone.ts"]).unwrap();

        fs::write(
            dir.path().join("src/app.ts"),
            "export function answer() { return 43; }\nexport function more() {}\n",
        )
        .unwrap();
        fs::remove_file(dir.path().join("src/gone.ts")).unwrap();
        let outcome = sync_changed_paths(dir.path(), &db, ["src/app.ts", "src/gone.ts"]).unwrap();

        let records = outcome.changed_records.expect("a sync hands its rows back");
        assert_eq!(
            records
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            outcome
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        let paths = IndexPaths::resolve(dir.path(), None).unwrap();
        let store =
            Store::open_for_read(&paths, Instant::now() + Duration::from_secs(10), || false)
                .unwrap();
        for (path, record) in &records {
            assert_eq!(record, &store.file_by_path(path).unwrap(), "{path}");
        }
        let app = records
            .iter()
            .find(|(path, _)| path == "src/app.ts")
            .and_then(|(_, record)| record.as_ref())
            .expect("the edited file keeps its row");
        assert_eq!(app.node_count, 3, "the row after the edit, with `more`");
        assert!(
            records
                .iter()
                .any(|(path, record)| path == "src/gone.ts" && record.is_none()),
            "the removed file has no row"
        );
    }

    fn stored_refs_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::resolved_refs::VERSION_KEY)
            .unwrap()
    }

    /// `files` under a full index: a sync, then the rebuild an outdated
    /// namespace escalates to, which writes the stored-references marker as
    /// every full index does (W5-09e).
    fn fully_indexed(name: &str, files: &[(&str, &str)]) -> (TestDir, PathBuf) {
        let dir = TestDir::new(name);
        write_files(dir.path(), files);
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(
            stored_refs_marker(&db),
            None,
            "a sync never writes the marker"
        );
        stage_outdated(&dir);
        sync_project_once(dir.path()).unwrap();
        assert!(
            stored_refs_marker(&db).is_some_and(|marker| marker.starts_with("1:")),
            "a full index writes the marker"
        );
        (dir, db)
    }

    const STORED_LIB: (&str, &str) = (
        "src/lib.ts",
        "export function helper(): number {\n  return 1;\n}\n\nexport function other(): number {\n  return 2;\n}\n",
    );
    const STORED_LIB_SHIFTED: (&str, &str) = (
        "src/lib.ts",
        "// one line down\nexport function helper(): number {\n  return 1;\n}\n\nexport function other(): number {\n  return 2;\n}\n",
    );
    const STORED_A: (&str, &str) = (
        "src/a.ts",
        "import { helper } from './lib';\n\nexport function a(): number {\n  return helper();\n}\n",
    );
    const STORED_B: (&str, &str) = (
        "src/b.ts",
        "import { helper, other } from './lib';\n\nexport function b(): number {\n  return helper() + other();\n}\n",
    );

    fn refresh(file: &str, stored: bool, whole: bool) -> DependentRefresh {
        DependentRefresh {
            file: file.to_string(),
            stored,
            whole,
        }
    }

    /// W5-09e: under the marker a full index wrote, the dependents of an
    /// edited file are re-resolved from their stored references, site by
    /// site, without being extracted; the graph is a fresh index's and the
    /// marker is back once the sync is persisted.
    #[test]
    fn an_unchanged_dependent_is_re_resolved_from_its_stored_references() {
        let (dir, db) = fully_indexed("stored-refs", &[STORED_LIB, STORED_A, STORED_B]);
        let marker = stored_refs_marker(&db);
        take_dependent_refreshes();

        write_files(dir.path(), &[STORED_LIB_SHIFTED]);
        sync_changed_paths(dir.path(), &db, ["src/lib.ts"]).unwrap();

        assert_eq!(
            take_dependent_refreshes(),
            [
                refresh("src/a.ts", true, false),
                refresh("src/b.ts", true, false)
            ]
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "stored-refs-fresh",
                &[STORED_LIB_SHIFTED, STORED_A, STORED_B]
            )
        );
        assert_eq!(stored_refs_marker(&db), marker, "the marker is back");
    }

    /// W5-09e: with no marker, or one a full index did not write for these
    /// inputs, a dependent is extracted as before, and the sync leaves the
    /// marker away.
    #[test]
    fn without_a_valid_marker_a_dependent_is_extracted() {
        let dir = TestDir::new("stored-refs-none");
        write_files(dir.path(), &[STORED_LIB, STORED_A, STORED_B]);
        let db = default_db_path(dir.path()).unwrap();
        sync_changed_paths(dir.path(), &db, ["src/lib.ts", "src/a.ts", "src/b.ts"]).unwrap();
        take_dependent_refreshes();

        write_files(dir.path(), &[STORED_LIB_SHIFTED]);
        sync_changed_paths(dir.path(), &db, ["src/lib.ts"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [
                refresh("src/a.ts", false, false),
                refresh("src/b.ts", false, false)
            ]
        );
        assert_eq!(stored_refs_marker(&db), None);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "stored-refs-none-fresh",
                &[STORED_LIB_SHIFTED, STORED_A, STORED_B]
            )
        );

        Store::open(&db)
            .unwrap()
            .set_project_metadata(crate::resolved_refs::VERSION_KEY, "1:stale")
            .unwrap();
        write_files(dir.path(), &[STORED_LIB]);
        sync_changed_paths(dir.path(), &db, ["src/lib.ts"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [
                refresh("src/a.ts", false, false),
                refresh("src/b.ts", false, false)
            ]
        );
        assert_eq!(stored_refs_marker(&db), None, "a stale marker is dropped");
    }

    /// W5-09e: a captured site with no stored reference sends its file down
    /// the whole-file lane, as a site the extraction cannot reproduce does.
    #[test]
    fn a_site_without_stored_references_takes_the_whole_file_lane() {
        let (dir, db) = fully_indexed("stored-refs-site", &[STORED_LIB, STORED_A, STORED_B]);
        let gone = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "DELETE FROM resolved_refs WHERE file_path = 'src/a.ts' AND reference_name = 'helper' AND reference_kind = 'calls'",
                [],
            )
            .unwrap();
        assert_eq!(
            gone, 1,
            "the call of `helper` in a.ts was resolved and kept"
        );
        take_dependent_refreshes();

        write_files(dir.path(), &[STORED_LIB_SHIFTED]);
        sync_changed_paths(dir.path(), &db, ["src/lib.ts"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [
                refresh("src/a.ts", true, true),
                refresh("src/b.ts", true, false)
            ]
        );
    }

    const GODOT_PROJECT: (&str, &str) = (
        "project.godot",
        "config_version=5\n\n[application]\n\nconfig/name=\"Fixture\"\n\n[autoload]\n\nGameFlow=\"*res://game_flow.gd\"\n",
    );
    const GODOT_FLOW: (&str, &str) = (
        "game_flow.gd",
        "extends Node\n\nfunc return_to_map() -> void:\n\tpass\n",
    );
    const GODOT_FLOW_SHIFTED: (&str, &str) = (
        "game_flow.gd",
        "extends Node\n\n# one line down\nfunc return_to_map() -> void:\n\tpass\n",
    );
    const GODOT_STAGE: (&str, &str) = (
        "stage.gd",
        "extends Node\n\nfunc _goto_map() -> void:\n\tGameFlow.return_to_map()\n",
    );
    const GODOT_STAGE_EDITED: (&str, &str) = (
        "stage.gd",
        "extends Node\n\nfunc _goto_map() -> void:\n\tGameFlow.return_to_map()\n\tGameFlow.return_to_map()\n",
    );
    const GODOT_HUD: (&str, &str) = (
        "hud.gd",
        "extends Node\n\nfunc _show_map() -> void:\n\tGameFlow.return_to_map()\n",
    );
    const GODOT_SCENE: (&str, &str) = (
        "main.tscn",
        "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://stage.gd\" id=\"1_stage\"]\n\n[node name=\"Main\" type=\"Node\"]\nscript = ExtResource(\"1_stage\")\n",
    );

    /// W5-09e: the marker digests the Godot DSL configuration a `.tres`
    /// extraction reads, so a changed configuration leaves the dependents
    /// to extraction and the marker away until the next full index.
    #[test]
    fn a_changed_godot_dsl_configuration_leaves_dependents_to_extraction() {
        let files = [GODOT_PROJECT, GODOT_FLOW, GODOT_STAGE, GODOT_SCENE];
        let (dir, db) = fully_indexed("stored-refs-dsl", &files);
        fs::write(
            index_paths(dir.path()).unwrap().extension_config(),
            r#"{ "godot": { "dsl": { "resourceFields": ["skill_effect"] } } }"#,
        )
        .unwrap();
        take_dependent_refreshes();

        write_files(dir.path(), &[GODOT_FLOW_SHIFTED]);
        sync_changed_paths(dir.path(), &db, ["game_flow.gd"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [refresh("stage.gd", false, false)]
        );
        assert_eq!(stored_refs_marker(&db), None);
    }

    /// W5-09e: a sync failing once a changed file's base references are
    /// written and before its framework references are, with the file's new
    /// hash already committed, leaves the marker away: later syncs extract
    /// their dependents until a full index writes it again.
    #[test]
    fn a_sync_failing_before_framework_references_leaves_the_marker_away() {
        let files = [
            GODOT_PROJECT,
            GODOT_FLOW,
            GODOT_STAGE,
            GODOT_HUD,
            GODOT_SCENE,
        ];
        let (dir, db) = fully_indexed("stored-refs-fail", &files);

        write_files(dir.path(), &[GODOT_STAGE_EDITED]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, ["stage.gd"]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(stored_refs_marker(&db), None);

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(stored_refs_marker(&db), None, "nothing changed");
        // `stage.gd` lost its framework references with the failure, and
        // with them its edge into `game_flow.gd`; `hud.gd` kept both.
        take_dependent_refreshes();
        write_files(dir.path(), &[GODOT_FLOW_SHIFTED]);
        sync_changed_paths(dir.path(), &db, ["game_flow.gd"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [refresh("hud.gd", false, false)]
        );
        assert_eq!(stored_refs_marker(&db), None);

        stage_outdated(&dir);
        sync_project_once(dir.path()).unwrap();
        assert!(stored_refs_marker(&db).is_some(), "a full index writes it");
    }

    const TABLE_PACKAGE: (&str, &str) = (
        "package.json",
        "{\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"6\"}}\n",
    );
    const TABLE_ADMIN: (&str, &str) = (
        "src/adminRoutes.js",
        "import Reports from './Reports';\n\nexport const adminRoutes = [{ path: 'reports', element: <Reports/> }];\n",
    );
    const TABLE_PAGE: (&str, &str) = (
        "src/Reports.js",
        "export default function Reports() {\n  return null;\n}\n",
    );
    const TABLE_LAYOUT: (&str, &str) = (
        "src/Layout.js",
        "export default function Layout() {\n  return null;\n}\n",
    );
    const MOUNTED_AT_ADMIN: (&str, &str) = (
        "src/routes.js",
        "import { Routes, Route } from 'react-router-dom';\nimport { adminRoutes } from './adminRoutes';\n\nexport default function Router() {\n  return <Routes><Route path=\"/admin\">{adminRoutes.map((route) => <Route key={route.path} {...route}/>)}</Route></Routes>;\n}\n",
    );
    const MOUNTED_AT_MANAGE: (&str, &str) = (
        "src/routes.js",
        "import { Routes, Route } from 'react-router-dom';\nimport { adminRoutes } from './adminRoutes';\n\nexport default function Router() {\n  return <Routes><Route path=\"/manage\">{adminRoutes.map((route) => <Route key={route.path} {...route}/>)}</Route></Routes>;\n}\n",
    );
    const SELF_MAPPED: (&str, &str) = (
        "src/table.js",
        "import { Routes, Route } from 'react-router-dom';\nimport Layout from './Layout';\nimport Reports from './Reports';\n\nexport const tableRoutes = [{ path: 'reports', element: <Reports/> }];\n\nexport function Shell() {\n  return <Routes><Route path=\"/\" element={<Layout/>}>{tableRoutes.map((route) => <Route key={route.path} {...route}/>)}</Route></Routes>;\n}\n",
    );
    const EARLIER_HANDS: (&str, &str) = (
        "src/a.js",
        "import { useRoutes } from 'react-router-dom';\nimport { tableRoutes } from './table';\n\nexport default function App() {\n  return useRoutes(tableRoutes);\n}\n",
    );
    const EARLIER_QUIET: (&str, &str) = (
        "src/a.js",
        "import { useNavigate } from 'react-router-dom';\n\nexport default function App() {\n  return null;\n}\n",
    );
    const NAV_TO_REPORTS: (&str, &str) = (
        "src/nav.js",
        "import { useNavigate } from 'react-router-dom';\nexport function Menu() {\n  const navigate = useNavigate();\n  const open = () => navigate('/reports');\n  return <button onClick={open} />;\n}\n",
    );

    /// The `references` edges out of the routes `/admin/reports`,
    /// `/manage/reports` and `/reports` of `db`, the table routes of the
    /// W7-09d tests.
    fn table_route_edges(db: &Path) -> Vec<String> {
        edge_projection(db)
            .into_iter()
            .filter(|edge| {
                let (source, _) = edge.split_once(" -[references]-> ").unwrap_or_default();
                source.ends_with("/reports")
            })
            .collect()
    }

    /// W7-09d: a sync that fails after the table pass replaced the route of a
    /// table another file mounts leaves the new route's reference pending
    /// and the marker away. The next sync, with nothing changed, re-reads
    /// every JavaScript file from its extraction, which keeps the references
    /// of the routes no single file's extraction makes, and resolves them as
    /// a fresh index does.
    #[test]
    fn a_sync_failing_after_the_table_pass_heals_on_the_next() {
        let initial = [TABLE_PACKAGE, TABLE_ADMIN, TABLE_PAGE, MOUNTED_AT_ADMIN];
        let (dir, db) = fully_indexed("table-pass-fail", &initial);
        assert_eq!(
            table_route_edges(&db),
            [
                "src/adminRoutes.js:src/adminRoutes.js::route:/admin/reports -[references]-> src/Reports.js:Reports {\"confidence\":0.9,\"resolvedBy\":\"import\"}"
            ]
        );

        write_files(dir.path(), &[MOUNTED_AT_MANAGE]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, ["src/routes.js"]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(stored_refs_marker(&db), None);
        assert!(
            table_route_edges(&db).is_empty(),
            "the route was replaced, its reference left pending"
        );

        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        let fresh = fresh_edges(
            "table-pass-fail-fresh",
            &[TABLE_PACKAGE, TABLE_ADMIN, TABLE_PAGE, MOUNTED_AT_MANAGE],
        );
        assert_eq!(edge_projection(&db), fresh);
        assert_eq!(table_route_edges(&db).len(), 1, "{fresh:#?}");
    }

    /// W7-09d: the orphan sweep runs the table pass before it resolves every
    /// pending reference, as a full index does, and a route it replaces puts
    /// the references whose edges reached it back among them: here the
    /// earlier file stopped handing the table over before a sync heard of
    /// it, so `/reports` keeps its id and gains its file's layout, and the
    /// navigation into it binds again.
    #[test]
    fn the_orphan_sweep_reconciles_the_tables_and_rebinds_what_reached_them() {
        let initial = [
            TABLE_PACKAGE,
            TABLE_PAGE,
            TABLE_LAYOUT,
            SELF_MAPPED,
            EARLIER_HANDS,
            NAV_TO_REPORTS,
        ];
        let (dir, db) = fully_indexed("table-pass-sweep", &initial);
        let navigates = |db: &Path| {
            edge_projection(db)
                .into_iter()
                .filter(|edge| edge.contains("-[navigates]->"))
                .count()
        };
        assert_eq!(navigates(&db), 1);
        assert_eq!(table_route_edges(&db).len(), 1, "no layout yet");

        // The earlier file stops; no sync is told.
        write_files(dir.path(), &[EARLIER_QUIET]);
        Store::open(&db)
            .unwrap()
            .set_resolution_incomplete()
            .unwrap();
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert!(
            !Store::open(&db)
                .unwrap()
                .is_resolution_incomplete()
                .unwrap()
        );
        assert_eq!(table_route_edges(&db).len(), 2, "the layout came");
        assert_eq!(navigates(&db), 1, "the navigation found the route again");

        // The sync that hears of it changes nothing more.
        sync_changed_paths(dir.path(), &db, ["src/a.js"]).unwrap();
        let fresh = fresh_edges(
            "table-pass-sweep-fresh",
            &[
                TABLE_PACKAGE,
                TABLE_PAGE,
                TABLE_LAYOUT,
                SELF_MAPPED,
                EARLIER_QUIET,
                NAV_TO_REPORTS,
            ],
        );
        assert_eq!(edge_projection(&db), fresh);
    }

    /// W7-09d: when the table pass makes a sync's first change — every file
    /// unchanged, another lane rebuilding — it withdraws the markers first,
    /// so a failure after it leaves the next sync to re-read every
    /// JavaScript file, and the replaced route's pending reference resolves.
    #[test]
    fn the_table_pass_withdraws_the_markers_before_its_first_change() {
        let initial = [TABLE_PACKAGE, TABLE_ADMIN, TABLE_PAGE, MOUNTED_AT_ADMIN];
        let (dir, db) = fully_indexed("table-pass-first", &initial);
        // An unchanged index whose Java lane rebuilds, and a mount that
        // moved under no sync's notice.
        crate::java_heads::withdraw_marker(&Store::open(&db).unwrap()).unwrap();
        write_files(dir.path(), &[MOUNTED_AT_MANAGE]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, Vec::<&str>::new());
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(surface_marker(&db), None, "the table pass withdrew it");

        sync_changed_paths(dir.path(), &db, ["src/routes.js"]).unwrap();
        let fresh = fresh_edges(
            "table-pass-first-fresh",
            &[TABLE_PACKAGE, TABLE_ADMIN, TABLE_PAGE, MOUNTED_AT_MANAGE],
        );
        assert_eq!(edge_projection(&db), fresh);
        assert_eq!(table_route_edges(&db).len(), 1);
    }

    /// W7-09d: the routes the table pass made compare equal to the ones it
    /// wants, their references with them, so a reconciled project plans no
    /// change, and a sync that touches no table neither replaces a route nor
    /// withdraws a marker. A sync's own reconciliation is one the next plan
    /// keeps too.
    #[test]
    fn a_reconciled_project_plans_no_table_change() {
        let initial = [
            TABLE_PACKAGE,
            TABLE_ADMIN,
            TABLE_PAGE,
            TABLE_LAYOUT,
            SELF_MAPPED,
            EARLIER_HANDS,
            NAV_TO_REPORTS,
            MOUNTED_AT_ADMIN,
        ];
        let (dir, db) = fully_indexed("table-pass-reconciled", &initial);
        let plans_a_change = || {
            let store = Store::open(&db).unwrap();
            let paths = index_paths(dir.path()).unwrap();
            let scope = ProjectScope::load(dir.path(), &paths).unwrap();
            let mut resolver = ReferenceResolver::new(dir.path().to_string_lossy())
                .with_max_file_size(scope.options.max_file_size);
            {
                let context = resolver.store_context(&store);
                resolver.initialize(&context);
            }
            resolver
                .plan_cross_file_nodes(&store)
                .unwrap()
                .changes_store()
        };
        assert_eq!(table_route_edges(&db).len(), 2, "/admin/reports, /reports");
        assert!(!plans_a_change(), "the full index reconciled every table");

        write_files(dir.path(), &[MOUNTED_AT_MANAGE]);
        sync_changed_paths(dir.path(), &db, ["src/routes.js"]).unwrap();
        assert!(!plans_a_change(), "the sync reconciled the moved mount");
        assert_eq!(table_route_edges(&db).len(), 2, "/manage/reports, /reports");
    }

    /// W5-09e: `project.godot`'s extraction maps `uid://` autoloads through
    /// every `.gd.uid` and `.tscn`, so it is extracted even under the
    /// marker. No resolution rule gives it an outgoing edge today, so no sync
    /// reaches it as a dependent: the refresh is driven directly.
    #[test]
    fn project_godot_is_extracted_even_under_the_marker() {
        let project = (
            "project.godot",
            "config_version=5\n\n[application]\n\nconfig/name=\"Fixture\"\n\n[autoload]\n\nGameFlow=\"*uid://gfscript\"\n",
        );
        let files = [
            project,
            GODOT_FLOW,
            ("game_flow.gd.uid", "uid://gfscript\n"),
            GODOT_STAGE,
            GODOT_SCENE,
        ];
        let (dir, db) = fully_indexed("stored-refs-godot", &files);
        let autoloads = |store: &Store| -> Vec<String> {
            store
                .stored_refs_of_file("project.godot")
                .unwrap()
                .into_iter()
                .filter(|reference| {
                    reference.reference_subkind
                        == Some(codegraph_core::types::ReferenceSubkind::Autoload)
                })
                .map(|reference| reference.reference_name)
                .collect()
        };
        let mut store = Store::open(&db).unwrap();
        assert_eq!(autoloads(&store), ["game_flow.gd"]);

        // The sidecar's uid moves; `project.godot` itself does not change.
        write_files(dir.path(), &[("game_flow.gd.uid", "uid://other\n")]);
        let paths = index_paths(dir.path()).unwrap();
        let scope = ProjectScope::load(dir.path(), &paths).unwrap();
        let mut resolver = ReferenceResolver::new(dir.path().to_string_lossy())
            .with_max_file_size(scope.options.max_file_size);
        {
            let context = resolver.store_context(&store);
            resolver.initialize(&context);
        }
        take_dependent_refreshes();
        refresh_dependent_refs(
            dir.path(),
            &mut store,
            &scope,
            &resolver,
            &BTreeMap::new(),
            &BTreeSet::from(["project.godot".to_string(), "stage.gd".to_string()]),
            &BTreeMap::new(),
            &HashSet::new(),
            true,
            &codegraph_resolve::CrossFilePlan::default(),
        )
        .unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [
                refresh("project.godot", false, true),
                refresh("stage.gd", true, true)
            ]
        );
        assert!(
            autoloads(&store).is_empty(),
            "the autoload the sidecar no longer names is gone"
        );
    }

    /// W5-09e: a parked path reference of `project.godot` (its main scene,
    /// before the scene exists) sends it down the whole-file lane once the
    /// file it names comes, and it is extracted there even under the marker.
    #[test]
    fn project_godot_reached_through_a_parked_path_is_extracted() {
        let project = (
            "project.godot",
            "config_version=5\n\n[application]\n\nconfig/name=\"Fixture\"\nrun/main_scene=\"res://scenes/main.tscn\"\n",
        );
        let (dir, db) = fully_indexed("stored-refs-godot-path", &[project, GODOT_FLOW]);
        take_dependent_refreshes();

        let scene = (
            "scenes/main.tscn",
            "[gd_scene format=3]\n\n[node name=\"Main\" type=\"Node\"]\n",
        );
        write_files(dir.path(), &[scene]);
        sync_changed_paths(dir.path(), &db, ["scenes/main.tscn"]).unwrap();
        assert_eq!(
            take_dependent_refreshes(),
            [refresh("project.godot", false, true)]
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "stored-refs-godot-path-fresh",
                &[project, GODOT_FLOW, scene]
            )
        );
    }

    /// W5-09e: under a full index, every file's stored references (its
    /// unresolved and resolved rows) are what extracting it again yields,
    /// row for row, across the languages and frameworks a sync reads.
    #[test]
    fn stored_references_are_each_files_extraction() {
        let files: &[(&str, &str)] = &[
            STORED_LIB,
            STORED_A,
            STORED_B,
            (
                "py/util.py",
                "class Greeter:\n    def greet(self, name):\n        return name\n\n\ndef helper():\n    return Greeter().greet('x')\n",
            ),
            (
                "py/main.py",
                "from util import helper, Greeter\n\n\ndef main():\n    helper()\n    return Greeter().greet('y')\n",
            ),
            (
                "cs/Lib.cs",
                "namespace App\n{\n    public enum Mode { A, B }\n\n    public static class Util\n    {\n        public static int Helper() => 1;\n    }\n}\n",
            ),
            (
                "cs/Use.cs",
                "using App;\n\nnamespace App.Use\n{\n    public class Runner\n    {\n        public Mode Mode = Mode.A;\n\n        public int Run() => Util.Helper();\n    }\n}\n",
            ),
            (
                "rs/lib.rs",
                "mod util;\n\nuse util::helper;\n\npub struct S;\n\nimpl S {\n    pub fn run(&self) -> u32 {\n        helper()\n    }\n}\n",
            ),
            ("rs/util.rs", "pub fn helper() -> u32 {\n    1\n}\n"),
            (
                "go/main.go",
                "package main\n\nfunc main() {\n\thelper()\n}\n",
            ),
            (
                "go/util.go",
                "package main\n\nfunc helper() int {\n\treturn 1\n}\n",
            ),
            (
                "java/App.java",
                "package app;\n\npublic class App {\n    public int run() {\n        return new Util().help();\n    }\n}\n",
            ),
            (
                "java/Util.java",
                "package app;\n\npublic class Util {\n    public int help() {\n        return 1;\n    }\n}\n",
            ),
            ("lua/main.lua", "local m = require('mod')\n\nreturn m.f()\n"),
            (
                "lua/mod.lua",
                "local M = {}\n\nfunction M.f()\n  return 1\nend\n\nreturn M\n",
            ),
            (
                "cpp/greeter.h",
                "class Greeter {\npublic:\n    Greeter();\n    void greet();\n};\n",
            ),
            (
                "cpp/main.cpp",
                "#include \"greeter.h\"\n\nint main() {\n    Greeter g;\n    g.greet();\n    Greeter* p = new Greeter();\n    p->greet();\n    return 0;\n}\n",
            ),
            (
                "php/A.php",
                "<?php\nnamespace App;\n\nclass A\n{\n    public function make(): B\n    {\n        return new B();\n    }\n}\n",
            ),
            ("php/B.php", "<?php\nnamespace App;\n\nclass B\n{\n}\n"),
            GODOT_PROJECT,
            GODOT_FLOW,
            GODOT_STAGE,
            GODOT_SCENE,
        ];
        let (dir, db) = fully_indexed("stored-refs-equal", files);
        let store = Store::open(&db).unwrap();
        let count = |table: &str| -> i64 {
            store
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert!(count("resolved_refs") > 0 && count("unresolved_refs") > 0);

        let paths = index_paths(dir.path()).unwrap();
        let scope = ProjectScope::load(dir.path(), &paths).unwrap();
        let mut resolver = ReferenceResolver::new(dir.path().to_string_lossy())
            .with_max_file_size(scope.options.max_file_size);
        {
            let context = resolver.store_context(&store);
            resolver.initialize(&context);
        }
        let rows = |refs: Vec<codegraph_core::types::UnresolvedRef>| -> Vec<String> {
            let mut rows = refs
                .iter()
                .map(|reference| serde_json::to_string(reference).unwrap())
                .collect::<Vec<_>>();
            rows.sort();
            rows
        };
        let mut compared = 0;
        for file in store.all_files().unwrap() {
            let extracted =
                extract_dependent_refs(dir.path(), &file.path, &scope, &resolver).unwrap();
            assert_eq!(
                rows(store.stored_refs_of_file(&file.path).unwrap()),
                rows(extracted),
                "{}",
                file.path
            );
            compared += 1;
        }
        assert_eq!(compared, files.len());
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

    // W7-19a0: a sync whose touched names are only framework nodes its
    // re-extraction left out and its passes put back retries no name.

    const RESTORE_PACKAGE: (&str, &str) = (
        "package.json",
        "{\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"6\"}}\n",
    );
    const RESTORE_PAGE: (&str, &str) = (
        "src/Reports.js",
        "export default function Reports() {\n  return null;\n}\n",
    );
    const RESTORE_ROUTES: (&str, &str) = (
        "src/routes.js",
        "import { Routes, Route } from 'react-router-dom';\nimport Reports from './Reports';\n\nexport default function Router() {\n  return <Routes><Route path=\"/reports\" element={<Reports/>}/></Routes>;\n}\n",
    );

    /// `file` with a comment appended: no node of its own moves.
    fn with_comment(file: (&str, &str)) -> (String, String) {
        (file.0.to_string(), format!("{}// edited\n", file.1))
    }

    fn write_owned(root: &Path, files: &[&(String, String)]) {
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
            .collect();
        write_files(root, &files);
    }

    /// The names the one sync of `paths` retried parked references by.
    fn retried_by(dir: &TestDir, db: &Path, paths: &[&str]) -> Vec<String> {
        take_retry_names();
        sync_changed_paths(dir.path(), db, paths.iter().copied()).unwrap();
        let mut syncs = take_retry_names();
        assert_eq!(syncs.len(), 1, "one sync: {syncs:?}");
        syncs.pop().unwrap()
    }

    fn restore_fixture(name: &str, extra: &[(&str, &str)]) -> (TestDir, PathBuf) {
        let dir = TestDir::new(name);
        let db = default_db_path(dir.path()).unwrap();
        let mut initial = vec![RESTORE_PACKAGE, RESTORE_PAGE, RESTORE_ROUTES];
        initial.extend_from_slice(extra);
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let routes = Store::open(&db).unwrap().nodes_by_name("/reports").unwrap();
        assert!(
            routes.iter().any(|node| node.kind == NodeKind::Route),
            "the fixture's route is a framework node: {routes:?}"
        );
        (dir, db)
    }

    /// Given a route file whose re-extraction leaves its React Router route
    /// out, when a comment is appended, the route comes back as it was: the
    /// sync retries no name, and its graph is a fresh index's.
    #[test]
    fn a_sync_that_only_puts_framework_nodes_back_retries_no_name() {
        let (dir, db) = restore_fixture("restore-only", &[]);
        let edited = with_comment(RESTORE_ROUTES);
        write_owned(dir.path(), &[&edited]);
        assert_eq!(
            retried_by(&dir, &db, &[RESTORE_ROUTES.0]),
            Vec::<String>::new()
        );
        let fresh = fresh_edges(
            "restore-only-fresh",
            &[RESTORE_PACKAGE, RESTORE_PAGE, (&edited.0, &edited.1)],
        );
        assert_eq!(edge_projection(&db), fresh);
    }

    /// A route whose path changed comes back as another node: the sync
    /// retries by the names its file's comparison touched.
    #[test]
    fn a_sync_that_changes_a_framework_node_retries_by_its_names() {
        let (dir, db) = restore_fixture("restore-changed", &[]);
        write_files(
            dir.path(),
            &[(
                RESTORE_ROUTES.0,
                &RESTORE_ROUTES.1.replace("\"/reports\"", "\"/report\""),
            )],
        );
        let retried = retried_by(&dir, &db, &[RESTORE_ROUTES.0]);
        assert!(retried.contains(&"/reports".to_string()), "{retried:?}");
    }

    /// A file the same sync removed touched its names through the removal
    /// lane, not a node comparison: the retry runs.
    #[test]
    fn a_removed_file_keeps_the_retry_of_a_sync_that_put_framework_nodes_back() {
        let spare = (
            "src/Spare.js",
            "export function Spare() {\n  return null;\n}\n",
        );
        let (dir, db) = restore_fixture("restore-removed", &[spare]);
        write_owned(dir.path(), &[&with_comment(RESTORE_ROUTES)]);
        fs::remove_file(dir.path().join(spare.0)).unwrap();
        let retried = retried_by(&dir, &db, &[RESTORE_ROUTES.0, spare.0]);
        assert!(retried.contains(&"Spare".to_string()), "{retried:?}");
        assert!(retried.contains(&"/reports".to_string()), "{retried:?}");
    }

    /// Another changed file whose own nodes held gains a route: the
    /// framework extraction made a node no comparison saw, so the retry
    /// runs.
    #[test]
    fn a_framework_node_another_changed_file_gains_keeps_the_retry() {
        let more = (
            "src/More.js",
            "import { Routes, Route } from 'react-router-dom';\n\nexport function More() {\n  return null;\n}\n",
        );
        let (dir, db) = restore_fixture("restore-gained", &[more]);
        assert!(
            Store::open(&db)
                .unwrap()
                .nodes_by_name("/more")
                .unwrap()
                .is_empty()
        );
        write_owned(dir.path(), &[&with_comment(RESTORE_ROUTES)]);
        write_files(
            dir.path(),
            &[(
                more.0,
                &more.1.replace(
                    "return null;",
                    "return <Routes><Route path=\"/more\" element={null}/></Routes>;",
                ),
            )],
        );
        let retried = retried_by(&dir, &db, &[RESTORE_ROUTES.0, more.0]);
        assert!(retried.contains(&"/reports".to_string()), "{retried:?}");
        assert!(
            Store::open(&db)
                .unwrap()
                .nodes_by_name("/more")
                .unwrap()
                .iter()
                .any(|node| node.kind == NodeKind::Route)
        );
    }

    /// The constant a route in an unchanged file is named from changes past
    /// the 100 characters its node records: the post-extract pass renames a
    /// node no comparison saw, so the retry runs.
    #[test]
    fn a_post_extract_rename_in_another_file_keeps_the_retry() {
        let paths = (
            "src/config/paths.js",
            "export const paths = {\n  app: { reports: { path: '/reports' }, settings: { path: '/settings' }, profile: { path: '/profile' }, root: { path: '/app' } },\n};\n",
        );
        let router = (
            "src/router.js",
            "import { createBrowserRouter } from 'react-router-dom';\nimport { paths } from './config/paths';\nimport Reports from './Reports';\n\nexport const router = createBrowserRouter([\n  { path: paths.app.root.path, element: <Reports /> },\n]);\n",
        );
        let (dir, db) = restore_fixture("restore-renamed", &[paths, router]);
        let route_named = |db: &Path, name: &str| {
            Store::open(db)
                .unwrap()
                .nodes_by_name(name)
                .unwrap()
                .iter()
                .any(|node| node.kind == NodeKind::Route && node.file_path == router.0)
        };
        assert!(route_named(&db, "/app"));
        write_owned(dir.path(), &[&with_comment(RESTORE_ROUTES)]);
        write_files(
            dir.path(),
            &[(paths.0, &paths.1.replace("'/app'", "'/dash'"))],
        );
        let retried = retried_by(&dir, &db, &[RESTORE_ROUTES.0, paths.0]);
        assert!(route_named(&db, "/dash"), "the pass renamed the route");
        assert!(retried.contains(&"/reports".to_string()), "{retried:?}");
    }

    /// An unchanged-looking file stops handing a table to the router: the
    /// cross-file pass changes the table's routes, so the retry runs.
    #[test]
    fn a_cross_file_route_change_keeps_the_retry() {
        let extra = (
            "src/extra.js",
            "import { Routes, Route } from 'react-router-dom';\nimport Reports from './Reports';\n\nexport function Extra() {\n  return <Routes><Route path=\"/extra\" element={<Reports/>}/></Routes>;\n}\n",
        );
        let dir = TestDir::new("restore-cross-file");
        let db = default_db_path(dir.path()).unwrap();
        let initial = [
            TABLE_PACKAGE,
            TABLE_PAGE,
            TABLE_LAYOUT,
            SELF_MAPPED,
            EARLIER_HANDS,
            extra,
        ];
        write_files(dir.path(), &initial);
        sync_changed_paths(dir.path(), &db, initial.iter().map(|(path, _)| *path)).unwrap();
        let before = table_route_edges(&db);
        write_owned(dir.path(), &[&with_comment(extra)]);
        write_files(
            dir.path(),
            &[(
                EARLIER_HANDS.0,
                &EARLIER_HANDS
                    .1
                    .replace("useRoutes(tableRoutes)", "useRoutes([])"),
            )],
        );
        let retried = retried_by(&dir, &db, &[extra.0, EARLIER_HANDS.0]);
        assert_ne!(table_route_edges(&db), before, "the table's routes moved");
        assert!(retried.contains(&"/extra".to_string()), "{retried:?}");
    }

    #[test]
    fn a_name_touched_outside_a_node_comparison_stops_the_restore_check() {
        let mut names = ChangedNames::default();
        names.note_all(Vec::<String>::new());
        names.note_any(Vec::<String>::new());
        assert!(!names.restored.abandoned, "no name, no touch");
        names.note_any(["x".to_string()]);
        assert!(names.restored.abandoned);
        let mut names = ChangedNames::default();
        names.note_all(["x".to_string()]);
        assert!(names.restored.abandoned);
    }

    #[test]
    fn the_restore_check_keeps_each_file_once_and_within_its_limit() {
        let node = |id: &str| Node {
            id: id.to_string(),
            kind: NodeKind::Route,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "src/a.js".to_string(),
            language: Language::JavaScript,
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
        };
        let before = vec![node("route:a"), node("route:b")];
        let mut touched = ChangedNames::default();
        touched.note_node_diff(&before, &[]);
        assert!(!touched.touches_nothing());
        let mut quiet = ChangedNames::default();
        quiet.note_node_diff(&before, &before);
        assert!(quiet.touches_nothing());

        let mut names = ChangedNames::default();
        names.note_restorable("src/a.js", &before, &quiet);
        assert!(
            names.restored.files.is_empty(),
            "nothing touched, nothing kept"
        );
        names.restored.nodes = RESTORE_CHECK_NODE_LIMIT - 2;
        names.note_restorable("src/a.js", &before, &touched);
        assert!(!names.restored.abandoned, "at the limit");
        assert_eq!(names.restored.files.len(), 1);
        names.note_restorable("src/b.js", &before[..1], &touched);
        assert!(names.restored.abandoned, "past the limit");
        assert!(names.restored.files.is_empty());

        let mut names = ChangedNames::default();
        names.note_restorable("src/a.js", &before, &touched);
        names.note_restorable("src/a.js", &before, &touched);
        assert!(names.restored.abandoned, "a file kept twice");
    }

    // ----- what an added, removed or resized file moves (W7-27b) -----------

    const W727_MAIN: (&str, &str) = (
        "src/main.ts",
        "import { helper } from './api';\n\nexport function main(): number {\n  return helper();\n}\n",
    );
    const W727_API: (&str, &str) = (
        "src/api.ts",
        "export function helper(): number {\n  return 2;\n}\n",
    );
    const W727_INDEX: (&str, &str) = (
        "src/api/index.ts",
        "export function helper(): number {\n  return 1;\n}\n",
    );

    /// W7-27b: a sync that adds `src/api.ts` ahead of the indexed
    /// `src/api/index.ts` and fails after its mutations leaves the import
    /// where it was, and the next sync sees no added file. That sync still
    /// moves the import: the JavaScript-family marker the failed one withdrew
    /// makes it reconcile every JavaScript-family file.
    #[test]
    fn an_import_candidate_a_failed_sync_added_moves_the_import_next_time() {
        let (dir, db) = fully_indexed("added-candidate-fail", &[W727_MAIN, W727_INDEX]);
        write_files(dir.path(), &[W727_API]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [W727_API.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(surface_marker(&db), None, "the failed sync withdrew it");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "added-candidate-fail-fresh",
                &[W727_MAIN, W727_INDEX, W727_API]
            )
        );
    }

    /// W7-27b: an added file that comes after a resolved import's answer
    /// (`src/api.d.ts` beside the indexed `src/api.ts` that `./api` takes)
    /// leaves the answer, so the importing file is not resolved again whole.
    /// The later candidate is a TypeScript file in a folder the index already
    /// has: one in a new folder moves the JavaScript control inputs (the
    /// manifests a framework's detection probes below every file), and a
    /// `.tsx` file can bring a framework in, either of which re-resolves
    /// every JavaScript-family file.
    #[test]
    fn an_added_later_import_candidate_sends_no_importer_whole() {
        const LATER: (&str, &str) = ("src/api.d.ts", "export declare function other(): number;\n");
        let (dir, db) = fully_indexed("later-candidate", &[W727_MAIN, W727_API]);
        take_dependent_refreshes();
        write_files(dir.path(), &[LATER]);
        sync_changed_paths(dir.path(), &db, [LATER.0]).unwrap();
        let refreshes = take_dependent_refreshes();
        assert!(
            !refreshes
                .iter()
                .any(|refresh| refresh.file == W727_MAIN.0 && refresh.whole),
            "{refreshes:?}"
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("later-candidate-fresh", &[W727_MAIN, W727_API, LATER])
        );
    }

    /// W7-27b: a header that comes beside an include another header makes
    /// sends the translation units through that header down the whole-file
    /// lane, since the walk may now take it, only when the index holds a
    /// function-like macro: with none, no call is a macro expansion, and
    /// `src/main.c`, which includes `src/wrap.h`, is not resolved again.
    #[test]
    fn a_translation_unit_goes_whole_for_an_added_header_only_beside_a_macro() {
        const MAIN: (&str, &str) = (
            "src/main.c",
            "#include \"wrap.h\"\n\nint main(void) {\n    return 0;\n}\n",
        );
        const WRAP: (&str, &str) = ("src/wrap.h", "#include \"trace.h\"\n");
        const BESIDE: (&str, &str) = ("src/trace.h", "int trace_depth(void);\n");
        for (macro_header, whole) in [
            (("include/trace.h", "int trace_level(void);\n"), false),
            (("include/trace.h", "#define TRACE(value) (value)\n"), true),
        ] {
            let (dir, db) = fully_indexed("macro-gate", &[MAIN, WRAP, macro_header]);
            take_dependent_refreshes();
            write_files(dir.path(), &[BESIDE]);
            sync_changed_paths(dir.path(), &db, [BESIDE.0]).unwrap();
            let refreshes = take_dependent_refreshes();
            assert_eq!(
                refreshes
                    .iter()
                    .any(|refresh| refresh.file == MAIN.0 && refresh.whole),
                whole,
                "{macro_header:?}: {refreshes:?}"
            );
            assert_eq!(
                edge_projection(&db),
                fresh_edges("macro-gate-fresh", &[MAIN, WRAP, macro_header, BESIDE])
            );
        }
    }

    const W723_PACKAGE: (&str, &str) = (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^6\"}}\n",
    );
    /// A data router whose loader picks `TeamPage` from `./pages/Team`: the
    /// route's reference is `import:./pages/Team#TeamPage`, whose stem is
    /// `Team`, and whose path tail (`Team#TeamPage`) no file's key matches.
    const W723_ROUTER: (&str, &str) = (
        "src/router.tsx",
        "import { createBrowserRouter } from 'react-router-dom';\nexport const router = createBrowserRouter([\n  {\n    path: '/team',\n    lazy: async () => {\n      const { TeamPage } = await import('./pages/Team');\n      return { Component: TeamPage };\n    },\n  },\n]);\n",
    );
    const W723_TEAM: (&str, &str) = (
        "src/pages/Team.tsx",
        "export function TeamPage() {\n  return null;\n}\n",
    );
    const W723_OTHER: (&str, &str) = ("src/pages/Other.tsx", "export const other = 1;\n");
    const W723_LIB: (&str, &str) = ("src/lib/util.ts", "export const util = 1;\n");

    /// W7-23a (upstream "re-opens nothing for a module no route loads"): a
    /// module no route loads sends no route file back, and neither does one
    /// whose keys a resolved route's module path shares (`src/lib/Team.ts`
    /// for `./pages/Team`) but which the path does not name: a resolved
    /// route reference goes back only when its module now resolves into the
    /// changed files or the barrels over them. The added files sit in a
    /// folder the index has, so the JavaScript control inputs stay.
    #[test]
    fn a_module_no_route_loads_sends_no_route_file_back() {
        let mut files = vec![W723_PACKAGE, W723_ROUTER, W723_TEAM, W723_OTHER, W723_LIB];
        let (dir, db) = fully_indexed("route-module-unrelated", &files);
        for added in [
            ("src/lib/Team.ts", "export const team = 1;\n"),
            (
                "src/lib/slug.ts",
                "export function slug(s: string) {\n  return s;\n}\n",
            ),
        ] {
            take_dependent_refreshes();
            write_files(dir.path(), &[added]);
            sync_changed_paths(dir.path(), &db, [added.0]).unwrap();
            let refreshes = take_dependent_refreshes();
            assert!(
                !refreshes
                    .iter()
                    .any(|refresh| refresh.file == W723_ROUTER.0),
                "{added:?}: {refreshes:?}"
            );
            files.push(added);
        }
        assert_eq!(
            edge_projection(&db),
            fresh_edges("route-module-unrelated-fresh", &files)
        );
    }

    /// W7-23a: a sync that adds the module a parked lazy route waits for and
    /// fails after its mutations leaves the route unlinked, and the next sync
    /// sees no added file. That sync still links it: the JavaScript-family
    /// marker the failed one withdrew makes it reconcile every
    /// JavaScript-family file.
    #[test]
    fn a_lazy_module_a_failed_sync_added_links_the_route_next_time() {
        let (dir, db) = fully_indexed(
            "route-module-fail",
            &[W723_PACKAGE, W723_ROUTER, W723_OTHER, W723_LIB],
        );
        write_files(dir.path(), &[W723_TEAM]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [W723_TEAM.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(surface_marker(&db), None, "the failed sync withdrew it");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "route-module-fail-fresh",
                &[W723_PACKAGE, W723_ROUTER, W723_TEAM, W723_OTHER, W723_LIB]
            )
        );
    }

    /// W7-28 (lane V's key): a `.js` file's `<Docs />` and a reference of
    /// the name that is no tag (`< Docs />`, a space after the `<`) ask
    /// React's lookup apart. Only the tag reads the module its lazy value
    /// loads, so that module's change sends back the tag's site alone; one
    /// lookup for both would ask the first, which reads no module.
    #[test]
    fn a_javascript_tag_and_a_namesake_that_is_no_tag_ask_the_lookup_apart() {
        let (dir, db) = fully_indexed(
            "lane-v-javascript-tag",
            &[
                (
                    "package.json",
                    "{\"name\":\"app\",\"dependencies\":{\"react\":\"^18\"}}\n",
                ),
                (
                    "src/App.js",
                    "import { lazy } from 'react';\nconst Docs = lazy(() => import('./pages/Docs'));\nexport function Preview() {\n  return < Docs />;\n}\nexport function App() {\n  return <Docs />;\n}\n",
                ),
                (
                    "src/pages/Docs.js",
                    "export function DocsPage() { return null; }\n",
                ),
            ],
        );
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let changed: BTreeSet<String> = ["src/pages/Docs.js".to_string()].into();
        let sites: BTreeSet<FileReferenceSite> = crate::route_modules::route_module_sites(
            &store,
            &resolver,
            &changed,
            || Ok(BTreeSet::new()),
            &HashSet::new(),
        )
        .unwrap()
        .into_iter()
        .collect();
        assert_eq!(w723_lines(&sites), [("src/App.js".to_string(), 7)]);
    }

    // W7-23b: the navigation calls a sync sends back when a route a call
    // may name appears, goes or moves (upstream #2425's
    // `reopenNavigationsFor`, `sync-navigation-retry.test.ts`).

    /// One React Router app of the upstream monorepo: its `/` route, and a
    /// home screen that navigates to `/login` and `/back`, which the app
    /// lacks, and to its `/`.
    fn w723_app(name: &str) -> [(String, String); 3] {
        [
            (
                format!("apps/{name}/package.json"),
                format!(
                    "{{\"name\":\"{name}\",\"dependencies\":{{\"react\":\"18\",\"react-router-dom\":\"6\"}}}}\n"
                ),
            ),
            (
                format!("apps/{name}/src/App.jsx"),
                "import { Routes, Route } from 'react-router-dom'\nimport { Home } from './Home'\nexport const App = () => (\n  <Routes>\n    <Route path=\"/\" element={<Home />} />\n  </Routes>\n)\n".to_string(),
            ),
            (
                format!("apps/{name}/src/Home.jsx"),
                "import { useNavigate } from 'react-router-dom'\nexport const Home = () => {\n  const navigate = useNavigate()\n  const go = () => navigate('/login')\n  const back = () => navigate('/back')\n  const home = () => navigate('/')\n  return <button onClick={go} onDoubleClick={back} onBlur={home}>Go</button>\n}\n".to_string(),
            ),
        ]
    }

    /// The upstream monorepo of two such apps, `web` and `admin`, indexed.
    fn w723_monorepo(name: &str) -> (TestDir, PathBuf) {
        let mut owned = vec![(
            "package.json".to_string(),
            "{\"name\":\"mono\",\"private\":true,\"workspaces\":[\"apps/*\"]}\n".to_string(),
        )];
        owned.extend(w723_app("web"));
        owned.extend(w723_app("admin"));
        let files: Vec<(&str, &str)> = owned
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        fully_indexed(name, &files)
    }

    /// A resolver for `root`, detected over `store` as a sync detects it.
    fn w723_resolver(root: &Path, store: &Store) -> ReferenceResolver {
        let mut resolver = ReferenceResolver::new(root.to_string_lossy());
        {
            let context = resolver.store_context(store);
            resolver.initialize(&context);
        }
        resolver
    }

    /// `route` moved to `path`: a route of its file the sync added.
    fn w723_route_at(route: &Node, path: &str) -> Node {
        let mut moved = route.clone();
        moved.id = format!("route:{}:{}:{path}", route.file_path, route.start_line);
        moved.name = path.to_string();
        moved
    }

    /// The route of `file`.
    fn w723_route_of(store: &Store, file: &str) -> Node {
        store
            .nodes_by_kind(NodeKind::Route)
            .unwrap()
            .into_iter()
            .find(|route| route.file_path == file)
            .unwrap_or_else(|| panic!("a route in {file}"))
    }

    /// Each site's file and line, sorted.
    fn w723_lines(sites: &BTreeSet<FileReferenceSite>) -> Vec<(String, i64)> {
        let mut lines: Vec<(String, i64)> = sites
            .iter()
            .map(|found| {
                (
                    found.file_path.clone(),
                    found.site.as_ref().expect("a site").line,
                )
            })
            .collect();
        lines.sort();
        lines
    }

    /// Upstream: a route sends back the navigation calls of the app it
    /// belongs to, the parked ones and the resolved one, and no other app's.
    #[test]
    fn a_route_sends_back_the_navigation_calls_of_its_own_app() {
        let (dir, db) = w723_monorepo("nav-own-app");
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let web = w723_route_of(&store, "apps/web/src/App.jsx");
        let sites = crate::navigation::reopened_by_routes(
            &store,
            &resolver,
            &[w723_route_at(&web, "/login")],
            &[],
            &BTreeSet::new(),
            &HashSet::new(),
        )
        .unwrap();
        let home = "apps/web/src/Home.jsx".to_string();
        assert_eq!(
            w723_lines(&sites),
            [(home.clone(), 4), (home.clone(), 5), (home, 6)]
        );
    }

    /// Upstream: the calls of a file the sync resolved anew stay as that
    /// resolution left them.
    #[test]
    fn a_file_the_sync_resolved_anew_keeps_its_navigation_calls() {
        let (dir, db) = w723_monorepo("nav-reindexed");
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let web = w723_route_of(&store, "apps/web/src/App.jsx");
        let reindexed: HashSet<String> = ["apps/web/src/Home.jsx".to_string()].into();
        let sites = crate::navigation::reopened_by_routes(
            &store,
            &resolver,
            &[w723_route_at(&web, "/login")],
            &[],
            &BTreeSet::new(),
            &reindexed,
        )
        .unwrap();
        assert!(sites.is_empty(), "{sites:?}");
    }

    /// No ceiling (OD-B42; upstream skips a name more calls share than 500):
    /// both apps' routes changed send back all six calls, and 501 calls of
    /// one name go back every one.
    #[test]
    fn every_navigation_call_a_route_may_answer_goes_back_however_many_share_a_name() {
        let (dir, db) = w723_monorepo("nav-both-apps");
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let changed: Vec<Node> = ["apps/web/src/App.jsx", "apps/admin/src/App.jsx"]
            .iter()
            .map(|file| w723_route_at(&w723_route_of(&store, file), "/login"))
            .collect();
        let sites = crate::navigation::reopened_by_routes(
            &store,
            &resolver,
            &changed,
            &[],
            &BTreeSet::new(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(sites.len(), 6, "{sites:?}");

        let mut calls = String::from(
            "import { useNavigate } from 'react-router-dom'\nexport const Many = () => {\n  const navigate = useNavigate()\n",
        );
        for call in 0..501 {
            calls.push_str(&format!("  navigate('/a{call}')\n"));
        }
        calls.push_str("  return null\n}\n");
        let (dir, db) = fully_indexed(
            "nav-501",
            &[
                (
                    "package.json",
                    "{\"name\":\"web\",\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"6\"}}\n",
                ),
                (
                    "src/App.jsx",
                    "import { Routes, Route } from 'react-router-dom'\nimport { Many } from './Many'\nexport const App = () => (\n  <Routes>\n    <Route path=\"/\" element={<Many />} />\n  </Routes>\n)\n",
                ),
                ("src/Many.jsx", calls.as_str()),
            ],
        );
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let app = w723_route_of(&store, "src/App.jsx");
        let sites = crate::navigation::reopened_by_routes(
            &store,
            &resolver,
            &[w723_route_at(&app, "/a0")],
            &[],
            &BTreeSet::new(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(sites.len(), 501);
    }

    /// Upstream: a route no router navigates to (a nested route's relative
    /// path) looks up no call at all.
    #[test]
    fn a_route_no_router_navigates_to_looks_up_no_call() {
        let (dir, db) = w723_monorepo("nav-relative");
        let store = Store::open(&db).unwrap();
        let resolver = w723_resolver(dir.path(), &store);
        let changed: Vec<Node> = store
            .nodes_by_kind(NodeKind::Route)
            .unwrap()
            .iter()
            .map(|route| w723_route_at(route, "login"))
            .collect();
        take_navigation_steps();
        let sites = crate::navigation::reopened_by_routes(
            &store,
            &resolver,
            &changed,
            &[],
            &BTreeSet::new(),
            &HashSet::new(),
        )
        .unwrap();
        assert!(sites.is_empty(), "{sites:?}");
        assert!(
            !take_navigation_steps().contains(&"calls"),
            "a lookup ran for no router's route"
        );
    }

    /// Upstream #2425's first case: a React Router 5 app whose cart screen
    /// pushes `/login?redirect=shipping`.
    const W723_CART_PACKAGE: (&str, &str) = (
        "package.json",
        "{\"name\":\"shop\",\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"5\"}}\n",
    );
    const W723_CART_APP: (&str, &str) = (
        "src/App.js",
        "import { BrowserRouter as Router, Route } from 'react-router-dom'\nimport LoginScreen from './screens/LoginScreen'\nimport CartScreen from './screens/CartScreen'\nconst App = () => (\n  <Router>\n    <Route path='/login' component={LoginScreen} />\n    <Route path='/cart' component={CartScreen} />\n  </Router>\n)\nexport default App\n",
    );
    const W723_LOGIN_SCREEN: (&str, &str) = (
        "src/screens/LoginScreen.js",
        "const LoginScreen = () => <div>Login</div>\nexport default LoginScreen\n",
    );
    const W723_CART_SCREEN: (&str, &str) = (
        "src/screens/CartScreen.js",
        "const CartScreen = ({ history }) => {\n  const checkout = () => {\n    history.push('/login?redirect=shipping')\n  }\n  return <button onClick={checkout}>Checkout</button>\n}\nexport default CartScreen\n",
    );

    /// Upstream "a sync that changes no route looks up no failed
    /// navigation": a body edit to the file the routes are declared in, and
    /// to an unrelated one.
    #[test]
    fn a_sync_that_changes_no_route_looks_up_no_navigation_call() {
        let files = [
            W723_CART_PACKAGE,
            W723_CART_APP,
            W723_LOGIN_SCREEN,
            W723_CART_SCREEN,
        ];
        let (dir, db) = fully_indexed("nav-no-route-change", &files);
        let app = W723_CART_APP
            .1
            .replace("export default App", "export default App // routes");
        let login = (
            W723_LOGIN_SCREEN.0,
            "const LoginScreen = () => <div>Sign in</div>\nexport default LoginScreen\n",
        );
        write_files(dir.path(), &[(W723_CART_APP.0, app.as_str()), login]);
        take_navigation_steps();
        let outcome =
            sync_changed_paths(dir.path(), &db, [W723_CART_APP.0, W723_LOGIN_SCREEN.0]).unwrap();
        assert_eq!(outcome.files_reindexed, 2);
        assert!(
            !take_navigation_steps().contains(&"calls"),
            "a sync that changed no route looked up navigation calls"
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "nav-no-route-change-fresh",
                &[
                    W723_CART_PACKAGE,
                    (W723_CART_APP.0, app.as_str()),
                    login,
                    W723_CART_SCREEN
                ]
            )
        );
    }

    /// Upstream "takes no route snapshot without a router that navigates":
    /// in a project no navigation router reads, a sync reads no route after
    /// its change and looks up no call.
    #[test]
    fn a_project_without_a_navigation_router_reads_no_navigation() {
        let (dir, db) = fully_indexed(
            "nav-no-router",
            &[
                ("package.json", "{\"name\":\"lib\"}\n"),
                ("src/a.ts", "export function a() { return 1; }\n"),
            ],
        );
        write_files(
            dir.path(),
            &[("src/a.ts", "export function a() { return 2; }\n")],
        );
        take_navigation_steps();
        let outcome = sync_changed_paths(dir.path(), &db, ["src/a.ts"]).unwrap();
        assert_eq!(outcome.files_reindexed, 1);
        assert_eq!(take_navigation_steps(), Vec::<&str>::new());
    }

    /// W7-23b: a sync that adds the route a parked call waits for and fails
    /// after its mutations leaves the call parked, and the next sync sees no
    /// changed file and no route snapshot. That sync still binds it: the
    /// JavaScript-family marker the failed one withdrew makes it reconcile
    /// every JavaScript-family file.
    #[test]
    fn a_route_a_failed_sync_added_binds_the_call_next_time() {
        let package = (
            "package.json",
            "{\"name\":\"app\",\"dependencies\":{\"react\":\"19\",\"@tanstack/react-router\":\"1\"}}\n",
        );
        let settings = (
            "src/routes/settings.tsx",
            "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/settings')({ component: Settings })\nfunction Settings() {\n  return null\n}\n",
        );
        let nav = (
            "src/lib/nav.ts",
            "export function open(navigate) {\n  navigate({ to: '/profile' })\n}\n",
        );
        let profile = (
            "src/routes/profile.tsx",
            "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/profile')({ component: Profile })\nfunction Profile() {\n  return null\n}\n",
        );
        let (dir, db) = fully_indexed("nav-fail", &[package, settings, nav]);
        write_files(dir.path(), &[profile]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [profile.0]);
        FAIL_BEFORE_RESOLUTION.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(surface_marker(&db), None, "the failed sync withdrew it");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges("nav-fail-fresh", &[package, settings, nav, profile])
        );
    }

    // ----- W7-31: lane B, the references through an import binding ---------

    const W731_LOGGER: &str = "export class Logger {\n  info(message: string) {\n    return message;\n  }\n  warn(message: string) {\n    return message;\n  }\n}\n";
    const W731_HELPER: (&str, &str) = ("src/__tests__/helpers/logger.ts", W731_LOGGER);
    const W731_CONSOLE: (&str, &str) = ("src/log/console.ts", W731_LOGGER);
    const W731_FILE: (&str, &str) = ("src/log/file.ts", W731_LOGGER);
    const W731_VALUE: (&str, &str) = (
        "src/logging.ts",
        "import { Logger } from './log/console';\n\nexport const logger = new Logger();\n",
    );
    const W731_VALUE_OF_FILE: (&str, &str) = (
        "src/logging.ts",
        "import { Logger } from './log/file';\n\nexport const logger = new Logger();\n",
    );
    const W731_VALUE_BODY_EDIT: (&str, &str) = (
        "src/logging.ts",
        "import { Logger } from './log/console';\n\nexport const logger = new Logger();\n\nexport function unrelated(): number {\n  return 1;\n}\n",
    );
    /// Two calls and a function reference written through the binding.
    const W731_APP: (&str, &str) = (
        "src/app.ts",
        "import { logger } from './logging';\n\nexport function run() {\n  logger.info('a');\n  logger.warn('b');\n  setTimeout(logger.info, 1);\n}\n",
    );
    const W731_LONELY: (&str, &str) = (
        "src/lonely.ts",
        "export function lonely(): number {\n  return 1;\n}\n",
    );
    const W731_LONELY_EDITED: (&str, &str) = (
        "src/lonely.ts",
        "export function lonely(): number {\n  return 2;\n}\n",
    );

    /// W7-31: an edit of a module no file binds selects nothing.
    #[test]
    fn w731_lane_b_selects_nothing_for_a_module_no_file_binds() {
        let files = [
            W731_HELPER,
            W731_CONSOLE,
            W731_FILE,
            W731_VALUE,
            W731_APP,
            W731_LONELY,
        ];
        let (dir, db) = fully_indexed("w731-lonely", &files);
        take_binding_lane();
        write_files(dir.path(), &[W731_LONELY_EDITED]);
        sync_changed_paths(dir.path(), &db, [W731_LONELY.0]).unwrap();
        assert_eq!(take_binding_lane(), []);
    }

    /// W7-31: an edit that re-points the value's class import sends every
    /// reference written through the binding back by site from its stored
    /// references (two calls and a function reference), and the graph is a
    /// fresh index's.
    #[test]
    fn w731_lane_b_takes_every_reference_through_a_binding() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-through", &files);
        take_binding_lane();
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-through-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31 (OD-B72): a body edit of the value's module leaves its import
    /// statements and value declarations alone, so lane B fires nothing, and
    /// the graph is a fresh index's.
    #[test]
    fn w731_lane_b_fires_nothing_for_a_body_edit() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-body", &files);
        take_binding_lane();
        write_files(dir.path(), &[W731_VALUE_BODY_EDIT]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), []);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-body-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_BODY_EDIT,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31 (OD-B72): the value's declaration changes (an annotation added,
    /// the answer the same): its facts moved, so lane B fires.
    #[test]
    fn w731_lane_b_fires_when_the_value_declaration_changes() {
        const ANNOTATED: (&str, &str) = (
            "src/logging.ts",
            "import { Logger } from './log/console';\n\nexport const logger: Logger = new Logger();\n",
        );
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-declaration", &files);
        take_binding_lane();
        write_files(dir.path(), &[ANNOTATED]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-declaration-fresh",
                &[W731_HELPER, W731_CONSOLE, W731_FILE, ANNOTATED, W731_APP]
            )
        );
    }

    /// W7-31 (OD-B72): an object literal's members are part of its value
    /// declaration (#1932): a member added moves the facts, and lane B
    /// fires for the call through the literal.
    #[test]
    fn w731_lane_b_fires_when_an_object_literal_member_changes() {
        const ONE: (&str, &str) = (
            "src/users/one.ts",
            "export function getUser(id: string) {\n  return id;\n}\n",
        );
        const API: (&str, &str) = (
            "src/api.ts",
            "import { getUser } from './users/one';\n\nexport const api = { getUser };\n",
        );
        const API_WIDER: (&str, &str) = (
            "src/api.ts",
            "import { getUser } from './users/one';\n\nexport const api = { getUser, fetchUser: getUser };\n",
        );
        const CALLER: (&str, &str) = (
            "src/app.ts",
            "import { api } from './api';\n\nexport function run() {\n  return api.getUser('x');\n}\n",
        );
        let (dir, db) = fully_indexed("w731-literal", &[ONE, API, CALLER]);
        take_binding_lane();
        write_files(dir.path(), &[API_WIDER]);
        sync_changed_paths(dir.path(), &db, [API.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(1))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges("w731-literal-fresh", &[ONE, API_WIDER, CALLER])
        );
    }

    /// W7-31 (OD-B72): #2644 reads the value's file's import mappings, a
    /// regex over its text: import text a comment holds ahead of the
    /// statement comes first and names the class. Only the mappings moved
    /// (no line did), and lane B fires.
    #[test]
    fn w731_lane_b_fires_when_an_import_mapping_changes() {
        const SPACED: (&str, &str) = (
            "src/logging.ts",
            "\nimport { Logger } from './log/console';\n\nexport const logger = new Logger();\n",
        );
        const COMMENTED: (&str, &str) = (
            "src/logging.ts",
            "// import { Logger } from './log/file';\nimport { Logger } from './log/console';\n\nexport const logger = new Logger();\n",
        );
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, SPACED, W731_APP];
        let (dir, db) = fully_indexed("w731-mapping", &files);
        take_binding_lane();
        write_files(dir.path(), &[COMMENTED]);
        sync_changed_paths(dir.path(), &db, [SPACED.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-mapping-fresh",
                &[W731_HELPER, W731_CONSOLE, W731_FILE, COMMENTED, W731_APP]
            )
        );
    }

    /// W7-31 (OD-B72): an exported interface of the value's name comes
    /// ahead of it and is what the named import binds now. Only the
    /// module's declarations moved, and lane B fires.
    #[test]
    fn w731_lane_b_fires_when_a_namesake_comes_ahead() {
        const NAMESAKE: (&str, &str) = (
            "src/logging.ts",
            "import { Logger } from './log/console';\nexport interface logger {}\nexport const logger = new Logger();\n",
        );
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-namesake", &files);
        take_binding_lane();
        write_files(dir.path(), &[NAMESAKE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-namesake-fresh",
                &[W731_HELPER, W731_CONSOLE, W731_FILE, NAMESAKE, W731_APP]
            )
        );
    }

    /// W7-31 (OD-B72): a barrel the value's file imports its class through
    /// re-points, and the value's file has a body edit in the same sync.
    /// Its facts did not move, and the surface lane re-resolves no
    /// re-extracted file, so lane B fires for it after all.
    #[test]
    fn w731_lane_b_fires_for_a_quiet_module_when_a_surface_moves() {
        const BARREL: (&str, &str) = ("src/log/index.ts", "export { Logger } from './console';\n");
        const BARREL_OF_FILE: (&str, &str) =
            ("src/log/index.ts", "export { Logger } from './file';\n");
        const VALUE: (&str, &str) = (
            "src/logging.ts",
            "import { Logger } from './log';\n\nexport const logger = new Logger();\n",
        );
        const VALUE_EDITED: (&str, &str) = (
            "src/logging.ts",
            "import { Logger } from './log';\n\nexport const logger = new Logger();\n\nexport function unrelated(): number {\n  return 1;\n}\n",
        );
        let files = [
            W731_HELPER,
            W731_CONSOLE,
            W731_FILE,
            BARREL,
            VALUE,
            W731_APP,
        ];
        let (dir, db) = fully_indexed("w731-quiet", &files);
        take_binding_lane();
        write_files(dir.path(), &[BARREL_OF_FILE, VALUE_EDITED]);
        sync_changed_paths(dir.path(), &db, [BARREL.0, VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-quiet-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    BARREL_OF_FILE,
                    VALUE_EDITED,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31 (OD-B72): a module with no binding-facts row (an index from
    /// before the rows) fires on a body edit, and the edit writes its row.
    #[test]
    fn w731_lane_b_fires_when_the_module_has_no_row() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-no-row", &files);
        let key = crate::binding_facts::row_key(W731_VALUE.0);
        let row = Store::open(&db)
            .unwrap()
            .get_project_metadata(&key)
            .unwrap();
        assert!(row.is_some(), "a full index writes the row");
        Store::open(&db)
            .unwrap()
            .delete_project_metadata(&key)
            .unwrap();
        take_binding_lane();
        write_files(dir.path(), &[W731_VALUE_BODY_EDIT]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            Store::open(&db)
                .unwrap()
                .get_project_metadata(&key)
                .unwrap(),
            row,
            "the body edit leaves the facts, so the row is the one a full index wrote"
        );
    }

    /// W7-31 (OD-B72): after edits, removals and additions, a sync leaves
    /// the binding-facts rows a fresh index writes.
    #[test]
    fn w731_binding_facts_rows_follow_the_sync() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-rows", &files);
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        fs::remove_file(dir.path().join(W731_CONSOLE.0)).unwrap();
        sync_changed_paths(dir.path(), &db, [W731_CONSOLE.0]).unwrap();
        let rows = |db: &Path| {
            Store::open(db)
                .unwrap()
                .project_metadata_with_prefix(crate::binding_facts::ROW_KEY_PREFIX)
                .unwrap()
        };
        let fresh = TestDir::new("w731-rows-fresh");
        let final_files = [W731_HELPER, W731_FILE, W731_VALUE_OF_FILE, W731_APP];
        write_files(fresh.path(), &final_files);
        let fresh_db = default_db_path(fresh.path()).unwrap();
        sync_changed_paths(
            fresh.path(),
            &fresh_db,
            final_files.iter().map(|(path, _)| *path),
        )
        .unwrap();
        stage_outdated(&fresh);
        sync_project_once(fresh.path()).unwrap();
        assert!(!rows(&db).is_empty());
        assert_eq!(rows(&db), rows(&fresh_db));
    }

    /// W7-31: without the stored-references marker (an index no full index
    /// wrote), lane B reads the importer's references from its extraction,
    /// as the dependent refresh does, and the call follows the value's class
    /// import by site.
    #[test]
    fn w731_lane_b_reads_an_importer_extraction_without_the_stored_references_marker() {
        let dir = TestDir::new("w731-no-marker");
        let db = default_db_path(dir.path()).unwrap();
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        write_files(dir.path(), &files);
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(stored_refs_marker(&db), None);
        take_binding_lane();
        take_dependent_refreshes();
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            take_dependent_refreshes(),
            [refresh("src/app.ts", false, false)]
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-no-marker-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31: a sync failing once the value's module is re-extracted, before
    /// lane B's sites are refreshed, leaves the JavaScript-family surface
    /// marker and the class-heads marker away, so the next sync reconciles
    /// every file of the lane's languages and the call follows the class
    /// import.
    #[test]
    fn w731_a_failed_sync_leaves_lane_b_to_the_next_one() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-fail", &files);
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [W731_VALUE.0]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(surface_marker(&db), None, "the failed sync withdrew it");
        assert_eq!(class_heads_marker(&db), None, "and this one");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-fail-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31: without the marker lane B reads no `resolved_refs` row: an
    /// index whose rows lack the importer's references still gives the three
    /// sites, from the importer's extraction.
    #[test]
    fn w731_lane_b_trusts_no_stored_reference_without_the_marker() {
        let dir = TestDir::new("w731-untrusted");
        let db = default_db_path(dir.path()).unwrap();
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        write_files(dir.path(), &files);
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(stored_refs_marker(&db), None);
        Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "DELETE FROM resolved_refs WHERE file_path = 'src/app.ts'",
                [],
            )
            .unwrap();
        take_binding_lane();
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), Some(3))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-untrusted-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31: under the marker, a binding site that holds no stored `imports`
    /// reference (its edge's column moved off the stored row) sends the
    /// importer whole, and the call follows the value's class import.
    #[test]
    fn w731_lane_b_sends_an_importer_whole_when_a_stored_binding_site_holds_no_import() {
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        let (dir, db) = fully_indexed("w731-unread-stored", &files);
        let moved = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "UPDATE edges SET col = 99 WHERE kind = 'imports' AND line = 1 AND col = 9 \
                 AND source = 'file:src/app.ts'",
                [],
            )
            .unwrap();
        assert_eq!(moved, 1, "the binding edge");
        take_binding_lane();
        take_dependent_refreshes();
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), None)]);
        assert_eq!(
            take_dependent_refreshes(),
            [refresh("src/app.ts", true, true)]
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-unread-stored-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    /// W7-31: without the marker, a binding site the importer's extraction
    /// holds no `imports` reference at (its edge's column moved) sends the
    /// importer whole too.
    #[test]
    fn w731_lane_b_sends_an_importer_whole_when_an_extracted_binding_site_holds_no_import() {
        let dir = TestDir::new("w731-unread-extracted");
        let db = default_db_path(dir.path()).unwrap();
        let files = [W731_HELPER, W731_CONSOLE, W731_FILE, W731_VALUE, W731_APP];
        write_files(dir.path(), &files);
        sync_changed_paths(dir.path(), &db, files.iter().map(|(path, _)| *path)).unwrap();
        assert_eq!(stored_refs_marker(&db), None);
        let moved = Store::open(&db)
            .unwrap()
            .connection()
            .execute(
                "UPDATE edges SET col = 99 WHERE kind = 'imports' AND line = 1 AND col = 9 \
                 AND source = 'file:src/app.ts'",
                [],
            )
            .unwrap();
        assert_eq!(moved, 1, "the binding edge");
        take_binding_lane();
        write_files(dir.path(), &[W731_VALUE_OF_FILE]);
        sync_changed_paths(dir.path(), &db, [W731_VALUE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("src/app.ts".to_string(), None)]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-unread-extracted-fresh",
                &[
                    W731_HELPER,
                    W731_CONSOLE,
                    W731_FILE,
                    W731_VALUE_OF_FILE,
                    W731_APP
                ]
            )
        );
    }

    const W731_PY_INIT: (&str, &str) = ("app/__init__.py", "");
    const W731_PY_LOGGERS: (&str, &str) = (
        "app/loggers.py",
        "class Logger:\n    def info(self, m):\n        return m\n\n\nclass Auditor:\n    def info(self, m):\n        return m\n",
    );
    const W731_PY_LOGMOD: (&str, &str) = (
        "app/logmod.py",
        "from app.loggers import Logger, Auditor\n\nlogger = Logger()\n",
    );
    const W731_PY_LOGMOD_AUDITOR: (&str, &str) = (
        "app/logmod.py",
        "from app.loggers import Logger, Auditor\n\nlogger = Auditor()\n",
    );
    const W731_PY_MAIN: (&str, &str) = (
        "app/main.py",
        "from app.logmod import logger\n\n\ndef run():\n    logger.info(\"start\")\n",
    );

    /// W7-31 (OD-B71): the same lane for Python: the call through the module
    /// global follows its class change, by site.
    #[test]
    fn w731_lane_b_takes_python_references_through_a_binding() {
        let files = [W731_PY_INIT, W731_PY_LOGGERS, W731_PY_LOGMOD, W731_PY_MAIN];
        let (dir, db) = fully_indexed("w731-python", &files);
        take_binding_lane();
        write_files(dir.path(), &[W731_PY_LOGMOD_AUDITOR]);
        sync_changed_paths(dir.path(), &db, [W731_PY_LOGMOD.0]).unwrap();
        assert_eq!(take_binding_lane(), [("app/main.py".to_string(), Some(1))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-python-fresh",
                &[
                    W731_PY_INIT,
                    W731_PY_LOGGERS,
                    W731_PY_LOGMOD_AUDITOR,
                    W731_PY_MAIN
                ]
            )
        );
    }

    /// W7-31 (OD-B72): two module-level assignments of the value trade
    /// places; the import binds the first, and every node keeps its line.
    /// Only the order of the value declarations moved, and lane B fires.
    #[test]
    fn w731_lane_b_fires_when_python_values_trade_places() {
        const TWICE: (&str, &str) = (
            "app/logmod.py",
            "from app.loggers import Logger, Auditor\n\nlogger = Logger()\nlogger = Auditor()\n",
        );
        const SWAPPED: (&str, &str) = (
            "app/logmod.py",
            "from app.loggers import Logger, Auditor\n\nlogger = Auditor()\nlogger = Logger()\n",
        );
        let files = [W731_PY_INIT, W731_PY_LOGGERS, TWICE, W731_PY_MAIN];
        let (dir, db) = fully_indexed("w731-python-order", &files);
        take_binding_lane();
        write_files(dir.path(), &[SWAPPED]);
        sync_changed_paths(dir.path(), &db, [TWICE.0]).unwrap();
        assert_eq!(take_binding_lane(), [("app/main.py".to_string(), Some(1))]);
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-python-order-fresh",
                &[W731_PY_INIT, W731_PY_LOGGERS, SWAPPED, W731_PY_MAIN]
            )
        );
    }

    /// W7-31: Python has no surface marker; the class-heads marker the
    /// failed sync withdrew sends every Python file whole through the next
    /// sync, so the call follows the class change there.
    #[test]
    fn w731_a_failed_sync_leaves_python_lane_b_to_the_next_one() {
        let files = [W731_PY_INIT, W731_PY_LOGGERS, W731_PY_LOGMOD, W731_PY_MAIN];
        let (dir, db) = fully_indexed("w731-python-fail", &files);
        write_files(dir.path(), &[W731_PY_LOGMOD_AUDITOR]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(true));
        let failed = sync_changed_paths(dir.path(), &db, [W731_PY_LOGMOD.0]);
        FAIL_BEFORE_FRAMEWORK_REFS.with(|fail| fail.set(false));
        assert!(failed.is_err(), "the injected failure must stop the sync");
        assert_eq!(class_heads_marker(&db), None, "the failed sync withdrew it");
        sync_changed_paths(dir.path(), &db, Vec::<&str>::new()).unwrap();
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "w731-python-fail-fresh",
                &[
                    W731_PY_INIT,
                    W731_PY_LOGGERS,
                    W731_PY_LOGMOD_AUDITOR,
                    W731_PY_MAIN
                ]
            )
        );
    }

    const TF_GATE_MAIN: (&str, &str) = (
        "main.tf",
        "module \"vpc\" {\n  source = \"./modules/vpc\"\n  cidr   = \"10.0.0.0/16\"\n}\n",
    );
    const TF_GATE_MAIN_COMMENTED: (&str, &str) = (
        "main.tf",
        "module \"vpc\" {\n  source = \"./modules/vpc\"\n  cidr   = \"10.0.0.0/16\"\n}\n# a comment\n",
    );
    const TF_GATE_USES: (&str, &str) = (
        "outputs.tf",
        "output \"vpc_id\" {\n  value = module.vpc.id\n}\n",
    );
    const TF_GATE_VARIABLES: (&str, &str) = ("modules/vpc/variables.tf", "variable \"cidr\" {}\n");
    const TF_GATE_OUTPUTS: (&str, &str) = (
        "modules/vpc/outputs.tf",
        "output \"id\" {\n  value = \"x\"\n}\n",
    );

    /// W7-24 cost (OD-B73): a Terraform edit that moves no module wiring (a
    /// comment line) re-resolves no file whole, though another file holds
    /// `module.vpc:` references, and the graph is a fresh index's.
    #[test]
    fn w724_b73_a_terraform_edit_moving_no_wiring_re_resolves_no_file_whole() {
        let files = [
            TF_GATE_MAIN,
            TF_GATE_USES,
            TF_GATE_VARIABLES,
            TF_GATE_OUTPUTS,
        ];
        let (dir, db) = fully_indexed("tf-gate-quiet", &files);
        take_dependent_refreshes();

        write_files(dir.path(), &[TF_GATE_MAIN_COMMENTED]);
        sync_changed_paths(dir.path(), &db, ["main.tf"]).unwrap();
        let whole: Vec<DependentRefresh> = take_dependent_refreshes()
            .into_iter()
            .filter(|refresh| refresh.whole)
            .collect();
        assert_eq!(whole, [], "no file is re-resolved whole");
        assert_eq!(
            edge_projection(&db),
            fresh_edges(
                "tf-gate-quiet-fresh",
                &[
                    TF_GATE_MAIN_COMMENTED,
                    TF_GATE_USES,
                    TF_GATE_VARIABLES,
                    TF_GATE_OUTPUTS
                ]
            )
        );
    }

    fn terraform_wiring_rows(db: &Path) -> Vec<(String, String)> {
        Store::open(db)
            .unwrap()
            .project_metadata_with_prefix(crate::terraform_wiring::ROW_KEY_PREFIX)
            .unwrap()
    }

    fn terraform_wiring_marker(db: &Path) -> Option<String> {
        Store::open(db)
            .unwrap()
            .get_project_metadata(crate::terraform_wiring::VERSION_KEY)
            .unwrap()
    }

    /// W7-24 cost (OD-B73): a full index writes a wiring row per Terraform
    /// file declaring a module, a variable or an output, and the marker. An
    /// index without the marker re-resolves the files holding `module.M:`
    /// references once and rebuilds the same rows; the next quiet edit
    /// re-resolves none.
    #[test]
    fn w724_b73_an_index_without_the_wiring_marker_rewires_once() {
        let files = [
            TF_GATE_MAIN,
            TF_GATE_USES,
            TF_GATE_VARIABLES,
            TF_GATE_OUTPUTS,
        ];
        let (dir, db) = fully_indexed("tf-gate-bootstrap", &files);
        let indexed = terraform_wiring_rows(&db);
        assert_eq!(
            indexed
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            [
                "terraform_wiring:main.tf",
                "terraform_wiring:modules/vpc/outputs.tf",
                "terraform_wiring:modules/vpc/variables.tf",
                "terraform_wiring:outputs.tf",
            ]
        );
        assert_eq!(
            terraform_wiring_marker(&db).as_deref(),
            Some(crate::terraform_wiring::VERSION)
        );
        {
            let store = Store::open(&db).unwrap();
            store
                .delete_project_metadata(crate::terraform_wiring::VERSION_KEY)
                .unwrap();
            store
                .delete_project_metadata_with_prefix(crate::terraform_wiring::ROW_KEY_PREFIX)
                .unwrap();
        }
        take_dependent_refreshes();

        write_files(dir.path(), &[TF_GATE_MAIN_COMMENTED]);
        sync_changed_paths(dir.path(), &db, ["main.tf"]).unwrap();
        let whole: Vec<String> = take_dependent_refreshes()
            .into_iter()
            .filter(|refresh| refresh.whole)
            .map(|refresh| refresh.file)
            .collect();
        assert_eq!(whole, ["outputs.tf"], "the bootstrap re-resolves it once");
        assert_eq!(
            terraform_wiring_marker(&db).as_deref(),
            Some(crate::terraform_wiring::VERSION)
        );
        assert_eq!(terraform_wiring_rows(&db), indexed, "the rows are rebuilt");

        write_files(dir.path(), &[TF_GATE_MAIN]);
        sync_changed_paths(dir.path(), &db, ["main.tf"]).unwrap();
        assert!(
            take_dependent_refreshes()
                .iter()
                .all(|refresh| !refresh.whole),
            "the marker is back: a quiet edit re-resolves nothing whole"
        );
        assert_eq!(
            edge_projection(&db),
            fresh_edges("tf-gate-bootstrap-fresh", &files)
        );
    }

    /// W7-24 cost (OD-B73): a sync writes its changed files' rows, so the
    /// only `module "vpc"`'s `source` moved away and back is seen both times
    /// by a use in another directory, which no other lane re-resolves.
    #[test]
    fn w724_b73_a_sync_keeps_the_wiring_rows_current() {
        let one = (
            "modules/vpc/outputs.tf",
            "output \"id\" {\n  value = \"1\"\n}\n",
        );
        let two = (
            "modules/vpc2/outputs.tf",
            "output \"id\" {\n  value = \"2\"\n}\n",
        );
        let infra =
            |source: &'static str| -> (&'static str, &'static str) { ("infra/main.tf", source) };
        let at_one = infra("module \"vpc\" {\n  source = \"../modules/vpc\"\n}\n");
        let at_two = infra("module \"vpc\" {\n  source = \"../modules/vpc2\"\n}\n");
        let app = (
            "app/main.tf",
            "output \"app_vpc\" {\n  value = module.vpc.id\n}\n",
        );
        let (dir, db) = fully_indexed("tf-gate-current", &[one, two, at_one, app]);
        for (round, main) in [(1, at_two), (2, at_one)] {
            write_files(dir.path(), &[main]);
            sync_changed_paths(dir.path(), &db, [main.0]).unwrap();
            assert_eq!(
                edge_projection(&db),
                fresh_edges(
                    &format!("tf-gate-current-fresh-{round}"),
                    &[one, two, main, app]
                ),
                "round {round}"
            );
        }
    }
}
