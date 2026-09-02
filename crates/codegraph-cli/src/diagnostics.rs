use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Args;
use codegraph_core::types::{ExtractionResult, Language};
use codegraph_extract::ExtractionStage;
use indicatif::ProgressBar;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const DIAGNOSTIC_SCHEMA_VERSION: u64 = 1;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const SLOW_FILE_AFTER: Duration = Duration::from_secs(5);
const SLOW_FILE_REPEAT: Duration = Duration::from_secs(30);
const STALLED_PROGRESS_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, Args)]
pub(crate) struct DiagnosticArgs {
    /// Record detailed lifecycle diagnostics for init/index/sync.
    #[arg(long)]
    pub(crate) debug: bool,
    /// Write diagnostics to this JSONL file. Implies --debug.
    #[arg(long, value_name = "FILE")]
    pub(crate) debug_log: Option<PathBuf>,
}

impl DiagnosticArgs {
    pub(crate) fn enabled(&self) -> bool {
        self.debug || self.debug_log.is_some()
    }
}

struct DiagnosticWriter {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl DiagnosticWriter {
    fn create(path: &Path, truncate: bool) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create diagnostic directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(truncate)
            .open(path)
            .with_context(|| format!("create diagnostic log {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Some(BufWriter::new(file)),
        })
    }

    fn write_value(&mut self, value: &Value) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .expect("diagnostic writer is open except during relocation");
        serde_json::to_writer(&mut *writer, value)?;
        writer.write_all(b"\n")?;
        // Debug mode favors crash survivability over throughput. A user who has
        // to terminate a stuck parser should still have the latest heartbeat.
        writer.flush()?;
        Ok(())
    }

    fn relocate(&mut self, destination: &Path) -> Result<()> {
        if self.path == destination {
            return Ok(());
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create diagnostic directory {}", parent.display()))?;
        }

        let old_path = self.path.clone();
        let mut writer = self
            .writer
            .take()
            .expect("diagnostic writer is open before relocation");
        writer.flush()?;
        drop(writer);

        if let Err(error) = fs::rename(&old_path, destination) {
            self.writer = Some(BufWriter::new(
                OpenOptions::new().append(true).open(&old_path)?,
            ));
            return Err(error).with_context(|| {
                format!(
                    "move diagnostic log {} to {}",
                    old_path.display(),
                    destination.display()
                )
            });
        }

        self.path = destination.to_path_buf();
        self.writer = Some(BufWriter::new(
            OpenOptions::new().append(true).open(destination)?,
        ));
        Ok(())
    }
}

struct DiagnosticInner {
    session_id: String,
    started: Instant,
    writer: Mutex<DiagnosticWriter>,
}

#[derive(Clone)]
pub(crate) struct DiagnosticSink {
    inner: Arc<DiagnosticInner>,
}

impl DiagnosticSink {
    fn create(path: &Path, truncate: bool) -> Result<Self> {
        let now_ms = now_millis();
        Ok(Self {
            inner: Arc::new(DiagnosticInner {
                session_id: format!("{}-{now_ms}", std::process::id()),
                started: Instant::now(),
                writer: Mutex::new(DiagnosticWriter::create(path, truncate)?),
            }),
        })
    }

    pub(crate) fn emit(&self, event: &str, fields: Value) {
        let mut object = match fields {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        object.insert(
            "schemaVersion".to_string(),
            Value::from(DIAGNOSTIC_SCHEMA_VERSION),
        );
        object.insert("timestamp".to_string(), Value::from(timestamp()));
        object.insert(
            "elapsedMs".to_string(),
            Value::from(self.inner.started.elapsed().as_millis() as u64),
        );
        object.insert(
            "sessionId".to_string(),
            Value::from(self.inner.session_id.clone()),
        );
        object.insert("event".to_string(), Value::from(event));
        let value = Value::Object(object);
        if let Ok(mut writer) = self.inner.writer.lock() {
            let _ = writer.write_value(&value);
        }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.inner
            .writer
            .lock()
            .map(|writer| writer.path.clone())
            .unwrap_or_default()
    }

    fn relocate(&self, destination: &Path) -> Result<()> {
        self.inner
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("diagnostic writer lock poisoned"))?
            .relocate(destination)
    }
}

pub(crate) struct DiagnosticRun {
    sink: Option<DiagnosticSink>,
    command: &'static str,
    auto_temporary: bool,
    final_path: Option<PathBuf>,
    finished: bool,
}

impl DiagnosticRun {
    pub(crate) fn disabled(command: &'static str) -> Self {
        Self {
            sink: None,
            command,
            auto_temporary: false,
            final_path: None,
            finished: false,
        }
    }

    pub(crate) fn start(
        project: &Path,
        index_root: &Path,
        command: &'static str,
        args: &DiagnosticArgs,
        fields: Value,
    ) -> Result<Self> {
        if !args.enabled() {
            return Ok(Self::disabled(command));
        }

        let stamp = file_stamp();
        let filename = format!("{command}-{stamp}-{}.jsonl", std::process::id());
        let (path, auto_temporary, final_path, truncate) = if let Some(path) = &args.debug_log {
            let requested_absolute = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| project.to_path_buf())
                    .join(path)
            };
            if !index_root.is_dir() && requested_absolute.starts_with(index_root) {
                (
                    project.join(format!(
                        ".codegraph-debug-{command}-{stamp}-{}",
                        std::process::id()
                    )),
                    true,
                    Some(path.clone()),
                    false,
                )
            } else {
                (path.clone(), false, None, true)
            }
        } else if index_root.is_dir() {
            (
                index_root.join("diagnostics").join(&filename),
                false,
                None,
                false,
            )
        } else {
            // Explicit init must not pre-create `.codegraph`: the rebuild layer
            // owns creation of the permanent lock namespace. Start beside it and
            // relocate only after begin_full_rebuild has established the root.
            (
                project.join(format!(
                    ".codegraph-debug-{command}-{stamp}-{}",
                    std::process::id()
                )),
                true,
                Some(index_root.join("diagnostics").join(&filename)),
                false,
            )
        };
        let sink = DiagnosticSink::create(&path, truncate)?;
        sink.emit("session_start", fields);
        Ok(Self {
            sink: Some(sink),
            command,
            auto_temporary,
            final_path,
            finished: false,
        })
    }

    pub(crate) fn sink(&self) -> Option<DiagnosticSink> {
        self.sink.clone()
    }

    pub(crate) fn path(&self) -> Option<PathBuf> {
        self.final_path
            .clone()
            .or_else(|| self.sink.as_ref().map(DiagnosticSink::path))
    }

    pub(crate) fn relocate_to_index_root(&mut self, index_root: &Path) {
        if !self.auto_temporary {
            return;
        }
        let destination = self.final_path.clone().unwrap_or_else(|| {
            index_root.join("diagnostics").join(format!(
                "{}-{}-{}.jsonl",
                self.command,
                file_stamp(),
                std::process::id()
            ))
        });
        if let Some(sink) = &self.sink {
            if let Err(error) = sink.relocate(&destination) {
                eprintln!("Warning: could not move debug log into the index directory: {error:#}");
            } else {
                self.auto_temporary = false;
                self.final_path = None;
            }
        }
    }

    pub(crate) fn phase_start(&self, phase: &str) {
        if let Some(sink) = &self.sink {
            sink.emit("phase_start", json!({ "phase": phase }));
        }
    }

    pub(crate) fn phase_end(&self, phase: &str, duration: Duration, fields: Value) {
        if let Some(sink) = &self.sink {
            let mut object = match fields {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            object.insert("phase".to_string(), Value::from(phase));
            object.insert(
                "durationMs".to_string(),
                Value::from(duration.as_millis() as u64),
            );
            sink.emit("phase_end", Value::Object(object));
        }
    }

    pub(crate) fn finish_success(&mut self, fields: Value) {
        if let Some(sink) = &self.sink {
            let mut object = match fields {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            object.insert("status".to_string(), Value::from("success"));
            sink.emit("session_end", Value::Object(object));
        }
        self.finished = true;
    }
}

impl Drop for DiagnosticRun {
    fn drop(&mut self) {
        if !self.finished
            && let Some(sink) = &self.sink
        {
            sink.emit("session_end", json!({ "status": "error" }));
            if self.auto_temporary {
                eprintln!("Debug log retained at: {}", sink.path().display());
            }
        }
    }
}

struct FileTrace {
    path: String,
    stage: &'static str,
    started: Instant,
    stage_started: Instant,
    stage_ms: BTreeMap<&'static str, u64>,
    size_bytes: Option<u64>,
    language: Option<String>,
    nodes: usize,
    edges: usize,
    references: usize,
    errors: usize,
    last_slow_report: Option<Instant>,
}

impl FileTrace {
    fn new(path: String) -> Self {
        let now = Instant::now();
        Self {
            path,
            stage: "scheduled",
            started: now,
            stage_started: now,
            stage_ms: BTreeMap::new(),
            size_bytes: None,
            language: None,
            nodes: 0,
            edges: 0,
            references: 0,
            errors: 0,
            last_slow_report: None,
        }
    }

    fn enter(&mut self, stage: &'static str) {
        let elapsed = self.stage_started.elapsed().as_millis() as u64;
        *self.stage_ms.entry(self.stage).or_default() += elapsed;
        self.stage = stage;
        self.stage_started = Instant::now();
        self.last_slow_report = None;
    }

    fn finish_stage(&mut self) {
        let elapsed = self.stage_started.elapsed().as_millis() as u64;
        *self.stage_ms.entry(self.stage).or_default() += elapsed;
        self.stage_started = Instant::now();
    }
}

struct TrackerState {
    scheduled: usize,
    parsed: usize,
    persisted: usize,
    buffered: usize,
    next_expected: usize,
    files: HashMap<usize, FileTrace>,
    last_persist: Instant,
    stopped: bool,
}

struct TrackerInner {
    state: Mutex<TrackerState>,
    wake: Condvar,
    bar: ProgressBar,
    sink: Option<DiagnosticSink>,
    base_message: String,
    tracking: bool,
}

#[derive(Clone)]
pub(crate) struct IndexTracker {
    inner: Arc<TrackerInner>,
}

pub(crate) struct IndexMonitor {
    inner: Arc<TrackerInner>,
    handle: Option<JoinHandle<()>>,
}

impl IndexTracker {
    pub(crate) fn start(
        bar: ProgressBar,
        sink: Option<DiagnosticSink>,
        base_message: String,
    ) -> (Self, IndexMonitor) {
        let tracking = sink.is_some() || !bar.is_hidden();
        let inner = Arc::new(TrackerInner {
            state: Mutex::new(TrackerState {
                scheduled: 0,
                parsed: 0,
                persisted: 0,
                buffered: 0,
                next_expected: 0,
                files: HashMap::new(),
                last_persist: Instant::now(),
                stopped: false,
            }),
            wake: Condvar::new(),
            bar,
            sink,
            base_message,
            tracking,
        });
        let handle = tracking.then(|| {
            let monitor_inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("codegraph-index-watchdog".to_string())
                .spawn(move || monitor_loop(monitor_inner))
                .expect("spawn index watchdog")
        });
        (
            Self {
                inner: Arc::clone(&inner),
            },
            IndexMonitor { inner, handle },
        )
    }

    pub(crate) fn scheduled(&self, index: usize, path: &str) {
        if !self.inner.tracking {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.scheduled += 1;
            state.files.insert(index, FileTrace::new(path.to_string()));
        }
    }

    pub(crate) fn stage(&self, index: usize, stage: &'static str) {
        if !self.inner.tracking {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(file) = state.files.get_mut(&index)
        {
            file.enter(stage);
        }
    }

    pub(crate) fn extraction_stage(&self, index: usize, stage: ExtractionStage) {
        let stage = match stage {
            ExtractionStage::DetectLanguage
            | ExtractionStage::Prepare
            | ExtractionStage::Embedded => "prepare",
            ExtractionStage::TreeSitterParse => "tree_sitter_parse",
            ExtractionStage::Walk => "walk",
        };
        self.stage(index, stage);
    }

    pub(crate) fn file_info(&self, index: usize, size_bytes: u64, language: Language) {
        if !self.inner.tracking {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(file) = state.files.get_mut(&index)
        {
            file.size_bytes = Some(size_bytes);
            file.language = Some(language.to_string());
        }
    }

    pub(crate) fn parsed(&self, index: usize, result: &ExtractionResult) {
        if !self.inner.tracking {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.parsed += 1;
            if let Some(file) = state.files.get_mut(&index) {
                file.nodes = result.nodes.len();
                file.edges = result.edges.len();
                file.references = result.unresolved_references.len();
                file.errors = result.errors.len();
                file.enter("buffered");
            }
        }
    }

    pub(crate) fn buffered(&self, buffered: usize) {
        if !self.inner.tracking {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.buffered = buffered;
        }
    }

    pub(crate) fn persisted(&self, index: usize) {
        if !self.inner.tracking {
            self.inner.bar.inc(1);
            return;
        }
        let trace = if let Ok(mut state) = self.inner.state.lock() {
            state.persisted += 1;
            state.next_expected = index + 1;
            state.last_persist = Instant::now();
            state.files.remove(&index)
        } else {
            None
        };
        self.inner.bar.inc(1);
        if let (Some(sink), Some(mut trace)) = (&self.inner.sink, trace) {
            trace.enter("persisted");
            trace.finish_stage();
            sink.emit(
                "file_complete",
                json!({
                    "fileIndex": index,
                    "file": trace.path,
                    "language": trace.language,
                    "sizeBytes": trace.size_bytes,
                    "durationMs": trace.started.elapsed().as_millis() as u64,
                    "stageDurationsMs": trace.stage_ms,
                    "nodes": trace.nodes,
                    "edges": trace.edges,
                    "references": trace.references,
                    "errors": trace.errors,
                }),
            );
        }
    }

    pub(crate) fn failed(&self, index: usize, _error: &anyhow::Error) {
        if !self.inner.tracking {
            return;
        }
        if let Some(sink) = &self.inner.sink {
            let file = self
                .inner
                .state
                .lock()
                .ok()
                .and_then(|state| state.files.get(&index).map(|file| file.path.clone()));
            sink.emit(
                "file_error",
                json!({
                    "fileIndex": index,
                    "file": file,
                    // Errors from filesystem APIs commonly contain absolute
                    // paths. Keep the machine-readable event useful without
                    // copying that potentially sensitive text into a feedback
                    // bundle.
                    "errorKind": "file_task_failed",
                }),
            );
        }
    }
}

impl IndexMonitor {
    pub(crate) fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.stopped = true;
            self.inner.wake.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IndexMonitor {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn monitor_loop(inner: Arc<TrackerInner>) {
    loop {
        let snapshot = {
            let Ok(state) = inner.state.lock() else {
                return;
            };
            let Ok((mut state, _)) = inner.wake.wait_timeout(state, HEARTBEAT_INTERVAL) else {
                return;
            };
            if state.stopped {
                return;
            }
            let now = Instant::now();
            let oldest_index = state
                .files
                .iter()
                .min_by_key(|(_, file)| file.started)
                .map(|(index, _)| *index);
            let oldest = oldest_index.and_then(|index| {
                let file = state.files.get(&index)?;
                Some((
                    index,
                    file.path.clone(),
                    file.stage,
                    file.started.elapsed(),
                    file.stage_started.elapsed(),
                ))
            });
            let waiting = state
                .files
                .get(&state.next_expected)
                .map(|file| (file.path.clone(), file.stage, file.stage_started.elapsed()));
            let mut slow_files = Vec::new();
            for (index, file) in &mut state.files {
                let stage_elapsed = file.stage_started.elapsed();
                let should_report = stage_elapsed >= SLOW_FILE_AFTER
                    && file
                        .last_slow_report
                        .is_none_or(|last| now.duration_since(last) >= SLOW_FILE_REPEAT);
                if should_report {
                    file.last_slow_report = Some(now);
                    slow_files.push((*index, file.path.clone(), file.stage, stage_elapsed));
                }
            }
            let active = state
                .files
                .values()
                .filter(|file| file.stage != "buffered")
                .count();
            (
                state.scheduled,
                state.parsed,
                state.persisted,
                state.buffered,
                state.next_expected,
                active,
                state.last_persist.elapsed(),
                oldest,
                waiting,
                slow_files,
            )
        };

        let (
            scheduled,
            parsed,
            persisted,
            buffered,
            next_expected,
            active,
            stalled,
            oldest,
            waiting,
            slow_files,
        ) = snapshot;
        if let Some(sink) = &inner.sink {
            sink.emit(
                "heartbeat",
                json!({
                    "scheduled": scheduled,
                    "active": active,
                    "parsed": parsed,
                    "buffered": buffered,
                    "persisted": persisted,
                    "nextExpected": next_expected,
                    "oldest": oldest.as_ref().map(|(index, path, stage, elapsed, stage_elapsed)| json!({
                        "fileIndex": index,
                        "file": path,
                        "stage": stage,
                        "elapsedMs": elapsed.as_millis() as u64,
                        "stageElapsedMs": stage_elapsed.as_millis() as u64,
                    })),
                }),
            );
            for (index, path, stage, elapsed) in &slow_files {
                sink.emit(
                    "slow_file",
                    json!({
                        "fileIndex": index,
                        "file": path,
                        "stage": stage,
                        "elapsedMs": elapsed.as_millis() as u64,
                    }),
                );
            }
        }

        if stalled >= STALLED_PROGRESS_AFTER {
            if let Some((path, stage, elapsed)) = waiting {
                inner.bar.set_message(format!(
                    "waiting for {path} — {stage} ({}s)",
                    elapsed.as_secs()
                ));
            }
        } else if inner.bar.message() != inner.base_message {
            inner.bar.set_message(inner.base_message.clone());
        }
    }
}

fn timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| now_millis().to_string())
}

fn file_stamp() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::ExtractionResult;
    use indicatif::ProgressBar;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "codegraph-diagnostics-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn diagnostic_args_debug_log_implies_enabled() {
        let args = DiagnosticArgs {
            debug: false,
            debug_log: Some(PathBuf::from("out.jsonl")),
        };
        assert!(args.enabled());
    }

    #[test]
    fn file_trace_accumulates_stage_durations() {
        let mut trace = FileTrace::new("src/A.java".to_string());
        trace.enter("metadata");
        trace.enter("read");
        trace.finish_stage();
        assert!(trace.stage_ms.contains_key("scheduled"));
        assert!(trace.stage_ms.contains_key("metadata"));
        assert!(trace.stage_ms.contains_key("read"));
    }

    #[test]
    fn progress_position_advances_only_after_persist() {
        let bar = ProgressBar::hidden();
        let (tracker, monitor) = IndexTracker::start(bar.clone(), None, "parsing".to_string());
        tracker.scheduled(0, "src/App.java");
        tracker.stage(0, "tree_sitter_parse");
        tracker.parsed(
            0,
            &ExtractionResult {
                nodes: Vec::new(),
                edges: Vec::new(),
                unresolved_references: Vec::new(),
                errors: Vec::new(),
                duration_ms: 0,
            },
        );
        assert_eq!(bar.position(), 0, "scheduled/parsed must not count as done");
        tracker.persisted(0);
        assert_eq!(bar.position(), 1);
        monitor.stop();
    }

    #[test]
    fn init_debug_starts_outside_index_root_then_relocates() {
        let project = temp_dir("relocate");
        let index_root = project.join(".codegraph");
        let args = DiagnosticArgs {
            debug: true,
            debug_log: None,
        };
        let mut run =
            DiagnosticRun::start(&project, &index_root, "init", &args, json!({})).unwrap();
        let temporary = run.sink().unwrap().path();
        let final_path = run.path().unwrap();

        assert!(temporary.is_file());
        assert!(!index_root.exists(), "debug must not pre-create .codegraph");
        assert!(final_path.starts_with(index_root.join("diagnostics")));

        fs::create_dir_all(&index_root).unwrap();
        run.relocate_to_index_root(&index_root);
        run.finish_success(json!({}));
        assert!(!temporary.exists());
        assert!(final_path.is_file());
        fs::remove_dir_all(project).ok();
    }

    #[test]
    fn init_custom_log_inside_missing_index_root_is_also_deferred() {
        let project = temp_dir("custom-relocate");
        let index_root = project.join(".codegraph");
        let requested = index_root.join("diagnostics/custom.jsonl");
        let args = DiagnosticArgs {
            debug: false,
            debug_log: Some(requested.clone()),
        };
        let mut run =
            DiagnosticRun::start(&project, &index_root, "init", &args, json!({})).unwrap();

        assert!(
            !index_root.exists(),
            "custom debug path must also be deferred"
        );
        assert_ne!(run.sink().unwrap().path(), requested);
        assert_eq!(run.path().unwrap(), requested);

        fs::create_dir_all(&index_root).unwrap();
        run.relocate_to_index_root(&index_root);
        run.finish_success(json!({}));
        assert!(requested.is_file());
        fs::remove_dir_all(project).ok();
    }

    #[test]
    fn jsonl_is_parseable_reports_slow_stage_and_never_logs_error_contents() {
        let project = temp_dir("jsonl");
        let path = project.join("debug.jsonl");
        let args = DiagnosticArgs {
            debug: false,
            debug_log: Some(path.clone()),
        };
        let mut run = DiagnosticRun::start(
            &project,
            &project.join(".codegraph"),
            "index",
            &args,
            json!({}),
        )
        .unwrap();
        let (tracker, monitor) =
            IndexTracker::start(ProgressBar::hidden(), run.sink(), "parsing".to_string());
        tracker.scheduled(0, "module/src/Slow.java");
        tracker.stage(0, "tree_sitter_parse");
        tracker.file_info(0, 42, Language::Java);
        {
            let mut state = tracker.inner.state.lock().unwrap();
            let file = state.files.get_mut(&0).unwrap();
            let old = Instant::now().checked_sub(Duration::from_secs(6)).unwrap();
            file.started = old;
            file.stage_started = old;
        }

        std::thread::sleep(HEARTBEAT_INTERVAL + Duration::from_millis(150));
        let result = ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: vec!["SUPER_SECRET_SOURCE_MARKER".to_string()],
            duration_ms: 0,
        };
        tracker.parsed(0, &result);
        tracker.buffered(0);
        tracker.persisted(0);
        monitor.stop();
        run.finish_success(json!({}));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("SUPER_SECRET_SOURCE_MARKER"));
        let events = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().all(|event| event["schemaVersion"] == 1));
        assert!(events.iter().any(|event| {
            event["event"] == "slow_file"
                && event["file"] == "module/src/Slow.java"
                && event["stage"] == "tree_sitter_parse"
        }));
        fs::remove_dir_all(project).ok();
    }
}
