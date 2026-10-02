//! `GET /api/events` — the viewer's live channel (server-sent events). Upstream
//! `src/ui-server/api/events.ts`.
//!
//! Two questions the open browser cannot answer for itself: has the file I am
//! looking at changed on disk (the drift banner — `/api/source` gives the
//! verdict, this stream says when to ask again), and has the index moved (the
//! live refresh — another process wrote the graph).
//!
//! **This server watches. It never syncs.** The project tree is observed
//! through `codegraph-watch`'s own `ProjectWatcher` in observe-only mode, so
//! the per-platform registration, the indexer's scope, symlink mapping, the
//! debounce and the degrade latch are the watcher's, and its sync closures are
//! never constructed. The index is observed through one non-recursive watch on
//! the index directory: a settled write is followed by one revision query, and
//! only a revision that moved becomes an event. Both watchers exist only while a
//! client is subscribed. Nothing polls.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use codegraph_core::IndexPaths;
use codegraph_watch::ProjectWatcher;
use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::server::AppState;
use crate::session;

/// Paths carried in one event; `total` is always the real number.
pub const MAX_EVENT_FILES: usize = 200;
/// Comment frame keeping the connection (and the client's idea of it) alive.
pub const HEARTBEAT: Duration = Duration::from_secs(25);
/// Quiet window before an index write is treated as finished.
const INDEX_SETTLE: Duration = Duration::from_millis(400);
/// Ceiling on that window, so a continuously-writing index still refreshes.
const INDEX_SETTLE_MAX: Duration = Duration::from_secs(3);
/// Debounce for source-file events.
const SOURCE_DEBOUNCE: Duration = Duration::from_millis(500);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `{lastIndexedAt, files}`, or `None` when there is no readable index.
type Revision = (Option<i64>, i64);

fn revision_json(revision: Revision) -> Value {
    json!({ "lastIndexedAt": revision.0, "files": revision.1 })
}

/// One SSE frame. `retry` on every frame so a reconnecting `EventSource`
/// backs off the way we asked.
fn frame(event: &Value) -> Bytes {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Bytes::from(format!("retry: 3000\nevent: {kind}\ndata: {event}\n\n"))
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<u64, mpsc::UnboundedSender<Bytes>>,
    next_id: u64,
    source: Option<ProjectWatcher>,
    index: Option<notify::RecommendedWatcher>,
    revision: Option<Revision>,
    source_up: bool,
    index_up: bool,
    degraded: Option<String>,
    closed: bool,
}

/// Fans filesystem and index changes out to every open viewer. One per
/// server; it owns the watchers, lazily.
#[derive(Clone, Default)]
pub struct EventHub {
    inner: Arc<Mutex<HubInner>>,
}

/// Removes its client when the response stream ends or is dropped.
struct ClientGuard {
    hub: EventHub,
    id: u64,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.hub.drop_client(self.id);
    }
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attached clients — for tests and for the watchers' lifetime.
    pub fn size(&self) -> usize {
        self.inner.lock().map(|i| i.clients.len()).unwrap_or(0)
    }

    /// Whether the source and index observers are up.
    pub fn watching(&self) -> (bool, bool) {
        self.inner
            .lock()
            .map(|i| (i.source_up, i.index_up))
            .unwrap_or((false, false))
    }

    /// Stop watching and end every open stream. Idempotent.
    pub fn close(&self) {
        let watchers = match self.inner.lock() {
            Ok(mut inner) => {
                inner.closed = true;
                // Dropping every sender ends every stream.
                inner.clients.clear();
                take_watchers(&mut inner)
            }
            Err(_) => return,
        };
        stop_watchers(watchers);
    }

    fn drop_client(&self, id: u64) {
        let watchers = match self.inner.lock() {
            Ok(mut inner) => {
                if inner.clients.remove(&id).is_none() || !inner.clients.is_empty() {
                    return;
                }
                take_watchers(&mut inner)
            }
            Err(_) => return,
        };
        stop_watchers(watchers);
    }

    fn broadcast(&self, event: &Value) {
        let bytes = frame(event);
        if let Ok(mut inner) = self.inner.lock() {
            inner.clients.retain(|_, tx| tx.send(bytes.clone()).is_ok());
        }
    }

    fn mark_degraded(&self, reason: String) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.degraded = Some(reason.clone());
            inner.source_up = false;
        }
        self.broadcast(&json!({ "type": "degraded", "reason": reason, "at": now_ms() }));
    }

    fn announce_changed(&self, paths: Option<Vec<String>>) {
        let scan = paths.is_none();
        let all = paths.unwrap_or_default();
        let files: Vec<&String> = all.iter().take(MAX_EVENT_FILES).collect();
        self.broadcast(&json!({
            "type": "changed",
            "files": files,
            "total": all.len(),
            "truncated": files.len() < all.len(),
            "scan": scan,
            "at": now_ms(),
        }));
    }
}

fn take_watchers(
    inner: &mut HubInner,
) -> (Option<ProjectWatcher>, Option<notify::RecommendedWatcher>) {
    inner.source_up = false;
    inner.index_up = false;
    (inner.source.take(), inner.index.take())
}

/// Outside the hub's lock: a watcher callback may be waiting on it, and
/// stopping joins the watcher's thread.
fn stop_watchers(watchers: (Option<ProjectWatcher>, Option<notify::RecommendedWatcher>)) {
    let (source, index) = watchers;
    // Dropping the index watcher ends its settle thread (the sender goes).
    drop(index);
    if let Some(source) = source {
        std::thread::spawn(move || source.stop());
    }
}

/// The current revision, or `None` when there is no readable index (a user can
/// delete the index with the viewer open; every endpoint says so on its own).
fn probe(project_root: &std::path::Path, paths: &IndexPaths) -> Option<Revision> {
    let store = session::open_store(project_root, paths).ok()?;
    store.index_revision().ok()
}

/// The project tree, through the engine's watcher with the sync taken out.
fn start_source_watcher(hub: &EventHub, project_root: &std::path::Path) -> Option<ProjectWatcher> {
    let on_paths = hub.clone();
    let on_scan = hub.clone();
    let on_degraded = hub.clone();
    let mut options = codegraph_watch::watch_options_for_project(project_root).ok()?;
    options.debounce = SOURCE_DEBOUNCE;
    options.on_degraded = Some(Arc::new(move |reason| on_degraded.mark_degraded(reason)));
    let options = options.observe_only(
        move |paths| on_paths.announce_changed(Some(paths)),
        move || on_scan.announce_changed(None),
    );
    // `None` when watching is off by policy (CODEGRAPH_NO_WATCH,
    // `watch.enabled = false`, a too-broad root) or the OS refused: the stream
    // stays, and `hello` says which half is live.
    ProjectWatcher::start(project_root, options).ok().flatten()
}

/// The index, through one non-recursive watch on its directory (a full
/// re-index REPLACES the database file, so a watch on the file itself would
/// follow the unlinked inode). Each event re-arms a settle window; the first
/// probe is deferred at most [`INDEX_SETTLE_MAX`].
fn start_index_watcher(
    hub: &EventHub,
    project_root: PathBuf,
    paths: IndexPaths,
) -> Option<notify::RecommendedWatcher> {
    let (poke, pokes) = std_mpsc::channel::<()>();
    let dir = paths.current_root().to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = poke.send(());
        }
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::NonRecursive).ok()?;
    let hub = hub.clone();
    std::thread::spawn(move || settle_loop(&hub, &pokes, &project_root, &paths));
    Some(watcher)
}

fn settle_loop(
    hub: &EventHub,
    pokes: &std_mpsc::Receiver<()>,
    project_root: &std::path::Path,
    paths: &IndexPaths,
) {
    // Blocks for the first write of a burst; ends when the watcher is dropped.
    while pokes.recv().is_ok() {
        let started = Instant::now();
        loop {
            let left = INDEX_SETTLE_MAX.saturating_sub(started.elapsed());
            match pokes.recv_timeout(INDEX_SETTLE.min(left)) {
                Ok(()) if !left.is_zero() => continue,
                Ok(()) | Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        check_index(hub, project_root, paths);
    }
}

/// One query. An unmoved revision is not an event.
fn check_index(hub: &EventHub, project_root: &std::path::Path, paths: &IndexPaths) {
    let previous = match hub.inner.lock() {
        Ok(inner) if !inner.closed && !inner.clients.is_empty() => inner.revision,
        _ => return,
    };
    let Some(next) = probe(project_root, paths) else {
        return;
    };
    if let Ok(mut inner) = hub.inner.lock() {
        inner.revision = Some(next);
    }
    if previous == Some(next) {
        return;
    }
    // Everything re-indexed since the mark held. A sync that only removed files
    // names nothing here — the revision comparison above decides the event.
    let (files, total) = match previous.and_then(|p| p.0) {
        Some(since) => session::open_store(project_root, paths)
            .ok()
            .and_then(|store| {
                store
                    .files_indexed_since(since, MAX_EVENT_FILES as i64)
                    .ok()
            })
            .unwrap_or_default(),
        None => (Vec::new(), 0),
    };
    let total = (total.max(0) as usize).max(files.len());
    hub.broadcast(&json!({
        "type": "index",
        "index": revision_json(next),
        "truncated": files.len() < total,
        "files": files,
        "total": total,
        "at": now_ms(),
    }));
}

fn stream_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // An event that sits in a proxy's buffer is an event that did not happen.
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
}

/// Attach one browser to the stream.
pub async fn subscribe(state: Arc<AppState>, method: &str) -> Response {
    let hub = state.events.clone();
    let is_head = method == "HEAD";
    let closed = hub.inner.lock().map(|i| i.closed).unwrap_or(true);
    if closed {
        // Shutting down: answer, do not attach.
        let body = if is_head {
            Body::empty()
        } else {
            Body::from(json!({ "error": "Shutting down.", "code": "internal" }).to_string())
        };
        let mut response = Response::new(body);
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }
    if is_head {
        let mut response = Response::new(Body::empty());
        stream_headers(&mut response);
        return response;
    }

    // Start what is not running yet. The probe and the watcher start touch the
    // disk, so they run off the async workers.
    let hub_for_start = hub.clone();
    let start_state = state.clone();
    let _ =
        tokio::task::spawn_blocking(move || ensure_watching(&hub_for_start, &start_state)).await;

    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    let id = {
        let Ok(mut inner) = hub.inner.lock() else {
            return Response::new(Body::empty());
        };
        let id = inner.next_id;
        inner.next_id += 1;
        let hello = json!({
            "type": "hello",
            "index": inner.revision.map(revision_json),
            "watching": { "source": inner.source_up, "index": inner.index_up },
            "degraded": inner.degraded,
            "heartbeatMs": HEARTBEAT.as_millis() as u64,
            "at": now_ms(),
        });
        let _ = tx.send(frame(&hello));
        inner.clients.insert(id, tx);
        id
    };
    let guard = ClientGuard {
        hub: hub.clone(),
        id,
    };
    let interval = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    let stream = futures_util::stream::unfold(
        (rx, interval, guard),
        |(mut rx, mut interval, guard)| async move {
            tokio::select! {
                next = rx.recv() => next.map(|bytes| (Ok::<Bytes, Infallible>(bytes), (rx, interval, guard))),
                // A comment frame: no client handler sees it; it notices a socket
                // the other end already dropped.
                _ = interval.tick() => Some((Ok(Bytes::from_static(b": ping\n\n")), (rx, interval, guard))),
            }
        },
    );
    let mut response = Response::new(Body::from_stream(stream));
    stream_headers(&mut response);
    response
}

fn ensure_watching(hub: &EventHub, state: &AppState) {
    let (need_revision, need_source, need_index) = match hub.inner.lock() {
        Ok(inner) if !inner.closed => (
            inner.revision.is_none(),
            inner.source.is_none(),
            inner.index.is_none(),
        ),
        _ => return,
    };
    let revision = if need_revision {
        probe(&state.project_root, &state.paths)
    } else {
        None
    };
    let source = if need_source {
        start_source_watcher(hub, &state.project_root)
    } else {
        None
    };
    let index = if need_index {
        start_index_watcher(hub, state.project_root.clone(), state.paths.clone())
    } else {
        None
    };
    let mut leftovers = (None, None);
    if let Ok(mut inner) = hub.inner.lock() {
        if inner.closed {
            leftovers = (source, index);
        } else {
            if need_revision {
                inner.revision = revision;
            }
            if need_source && inner.source.is_none() {
                inner.source_up = source.is_some();
                inner.source = source;
            } else {
                leftovers.0 = source;
            }
            if need_index && inner.index.is_none() {
                inner.index_up = index.is_some();
                inner.index = index;
            } else {
                leftovers.1 = index;
            }
        }
    }
    stop_watchers(leftovers);
}
