use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::policy::{watch_disabled_reason, WatchPolicy};
use crate::sync::{default_db_path, sync_changed_paths, SyncOutcome};

type SyncCallback = Arc<dyn Fn(SyncOutcome) + Send + Sync>;
type SyncFn = Arc<dyn Fn(Vec<String>) -> Result<SyncOutcome> + Send + Sync>;

#[derive(Clone)]
pub struct WatchOptions {
    pub debounce: Duration,
    pub no_watch: bool,
    pub db_path: Option<PathBuf>,
    pub inert_for_tests: bool,
    pub on_sync_complete: Option<SyncCallback>,
    sync_fn: Option<SyncFn>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            // Upstream default debounce is 2000ms (`watch-policy.ts` notes and
            // `watcher.ts:86-90,220-223`); env override is clamped [100ms, 60s].
            debounce: debounce_from_env(),
            no_watch: false,
            db_path: None,
            inert_for_tests: false,
            on_sync_complete: None,
            sync_fn: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    pub path: String,
    pub first_seen_ms: u128,
    pub last_seen_ms: u128,
}

pub struct ProjectWatcher {
    tx: Sender<LoopMessage>,
    thread: Option<JoinHandle<()>>,
    watcher: Option<RecommendedWatcher>,
}

pub fn start_serve_watcher(
    project_root: impl AsRef<Path>,
    options: WatchOptions,
) -> Result<Option<ProjectWatcher>> {
    ProjectWatcher::start(project_root, options)
}

impl ProjectWatcher {
    pub fn start(project_root: impl AsRef<Path>, options: WatchOptions) -> Result<Option<Self>> {
        let project_root = project_root.as_ref().to_path_buf();
        if watch_disabled_reason(&project_root, options.no_watch).is_some() {
            return Ok(None);
        }
        let policy = WatchPolicy::new(&project_root);
        let db_path = options
            .db_path
            .clone()
            .unwrap_or_else(|| default_db_path(&project_root));
        let sync_fn = options.sync_fn.clone().unwrap_or_else(|| {
            let project_root = project_root.clone();
            Arc::new(move |paths| sync_changed_paths(&project_root, &db_path, paths))
        });
        let (tx, rx) = mpsc::channel();
        let loop_policy = policy.clone();
        let on_sync_complete = options.on_sync_complete.clone();
        let debounce = options.debounce;
        let thread = thread::spawn(move || {
            event_loop(rx, loop_policy, debounce, sync_fn, on_sync_complete);
        });

        let watcher = if options.inert_for_tests {
            None
        } else {
            let callback_tx = tx.clone();
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                if let Ok(event) = event {
                    let _ = callback_tx.send(LoopMessage::Event(event.paths));
                }
            })?;
            // Unlike the upstream platform split (`watcher.ts:283-384`), notify v6
            // owns the OS-specific recursion strategy behind this recursive watch.
            watcher
                .watch(&project_root, RecursiveMode::Recursive)
                .with_context(|| format!("watch {}", project_root.display()))?;
            Some(watcher)
        };

        Ok(Some(Self {
            tx,
            thread: Some(thread),
            watcher,
        }))
    }

    pub fn ingest_event_for_tests(&self, relative: impl Into<PathBuf>) {
        let _ = self.tx.send(LoopMessage::Event(vec![relative.into()]));
    }

    pub fn pending_files(&self) -> Vec<PendingFile> {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(LoopMessage::Snapshot(tx));
        rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default()
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        let _ = self.watcher.take();
        let _ = self.tx.send(LoopMessage::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ProjectWatcher {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

enum LoopMessage {
    Event(Vec<PathBuf>),
    Snapshot(Sender<Vec<PendingFile>>),
    Stop,
}

#[derive(Debug, Clone)]
struct PendingInfo {
    first_seen_ms: u128,
    last_seen_ms: u128,
}

fn event_loop(
    rx: mpsc::Receiver<LoopMessage>,
    policy: WatchPolicy,
    debounce: Duration,
    sync_fn: SyncFn,
    on_sync_complete: Option<SyncCallback>,
) {
    let mut pending = BTreeMap::<String, PendingInfo>::new();
    let mut deadline = None::<Instant>;
    loop {
        let message = match deadline {
            Some(when) => match rx.recv_timeout(when.saturating_duration_since(Instant::now())) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };

        match message {
            Some(LoopMessage::Event(paths)) => {
                for path in paths {
                    if let Some(relative) = policy.normalize_relative(&path) {
                        if policy.should_handle_file(&relative)
                            || (policy.allows_file_path(&relative)
                                && maybe_deleted_source(&relative))
                        {
                            let now = epoch_millis();
                            pending
                                .entry(relative)
                                .and_modify(|info| info.last_seen_ms = now)
                                .or_insert(PendingInfo {
                                    first_seen_ms: now,
                                    last_seen_ms: now,
                                });
                        }
                    }
                }
                if !pending.is_empty() {
                    // Resetting the timer on every event ports the upstream exactly-once
                    // burst semantics (`upstream sync/watcher.ts:529-540`).
                    deadline = Some(Instant::now() + debounce);
                }
            }
            Some(LoopMessage::Snapshot(reply)) => {
                let _ = reply.send(snapshot(&pending));
            }
            Some(LoopMessage::Stop) => break,
            None => {
                let paths = pending.keys().cloned().collect::<Vec<_>>();
                pending.clear();
                deadline = None;
                if let Ok(outcome) = sync_fn(paths) {
                    if let Some(callback) = &on_sync_complete {
                        callback(outcome);
                    }
                }
            }
        }
    }
}

fn snapshot(pending: &BTreeMap<String, PendingInfo>) -> Vec<PendingFile> {
    pending
        .iter()
        .map(|(path, info)| PendingFile {
            path: path.clone(),
            first_seen_ms: info.first_seen_ms,
            last_seen_ms: info.last_seen_ms,
        })
        .collect()
}

fn maybe_deleted_source(relative: &str) -> bool {
    static SOURCE_EXTENSIONS: &[&str] = &[
        "ts",
        "tsx",
        "js",
        "jsx",
        "py",
        "go",
        "rs",
        "java",
        "c",
        "h",
        "cpp",
        "cc",
        "cxx",
        "hpp",
        "hxx",
        "cs",
        "php",
        "rb",
        "swift",
        "kt",
        "kts",
        "dart",
        "vue",
        "svelte",
        "liquid",
        "pas",
        "dpr",
        "dpk",
        "lpr",
        "dfm",
        "fmx",
        "scala",
        "sc",
        "lua",
        "luau",
        "m",
        "mm",
        "yml",
        "yaml",
        "twig",
        "xml",
        "properties",
    ];
    relative
        .rsplit_once('.')
        .is_some_and(|(_, ext)| SOURCE_EXTENSIONS.contains(&ext))
}

fn debounce_from_env() -> Duration {
    let millis = std::env::var("CODEGRAPH_WATCH_DEBOUNCE_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(2_000)
        .clamp(100, 60_000);
    Duration::from_millis(millis)
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    #[test]
    fn rapid_save_burst_triggers_exactly_one_reindex() {
        let dir = crate::sync::tests::TestDir::new("watch-debounce");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let db = crate::sync::default_db_path(dir.path());
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                inert_for_tests: true,
                db_path: Some(db),
                on_sync_complete: Some(Arc::new(move |outcome| {
                    seen.lock().unwrap().push(outcome);
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        fs::write(
            dir.path().join("src/app.ts.__tmp"),
            "export function one() { return 1; }\n",
        )
        .unwrap();
        fs::rename(
            dir.path().join("src/app.ts.__tmp"),
            dir.path().join("src/app.ts"),
        )
        .unwrap();
        fs::write(
            dir.path().join("src/app.ts"),
            "export function one() { return 1; }\n",
        )
        .unwrap();
        watcher.ingest_event_for_tests("src/app.ts.__tmp");
        watcher.ingest_event_for_tests("src/app.ts");
        watcher.ingest_event_for_tests("src/app.ts");
        std::thread::sleep(Duration::from_millis(220));
        watcher.stop();

        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].files_reindexed, 1);
        assert_eq!(outcomes[0].files_checked, 1);
    }

    #[test]
    fn ignored_directory_event_does_not_schedule_reindex() {
        let dir = crate::sync::tests::TestDir::new("watch-ignore");
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(
            dir.path().join("node_modules/pkg/index.ts"),
            "export const ignored = 1;\n",
        )
        .unwrap();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        let watcher = ProjectWatcher::start(
            dir.path(),
            WatchOptions {
                debounce: Duration::from_millis(50),
                inert_for_tests: true,
                on_sync_complete: Some(Arc::new(move |outcome| {
                    seen.lock().unwrap().push(outcome);
                })),
                ..WatchOptions::default()
            },
        )
        .unwrap()
        .unwrap();

        watcher.ingest_event_for_tests("node_modules/pkg/index.ts");
        std::thread::sleep(Duration::from_millis(150));
        watcher.stop();
        assert!(outcomes.lock().unwrap().is_empty());
    }
}
