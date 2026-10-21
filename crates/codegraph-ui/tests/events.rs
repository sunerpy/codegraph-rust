//! The live channel and `?ondrift=` — a port of upstream
//! `__tests__/ui-events-api.test.ts` (`v1.6.1`). The stream is read frame by
//! frame off the router's streaming body; every wait is bounded and fails with
//! what it saw.
//!
//! Every step that can block runs under its own named limit: indexing the
//! fixture, starting the viewer, connecting to the stream, each awaited event,
//! each request, a sync by another writer, closing the hub, the teardown and
//! the runtime's shutdown. A hang therefore fails with the step's name instead
//! of holding a CI job; this file once held a Windows job for 40 minutes.

mod support;

use std::future::Future;
use std::panic;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use support::{PORT, Project, Reply, Viewer, encode};

const ORIGINAL: &str = "export function greet(name: string): string {
  return 'hello ' + name;
}

export function shout(name: string): string {
  return greet(name).toUpperCase();
}
";

/// The wait for one event.
const WAIT: Duration = Duration::from_secs(15);
/// Any other step: generous for a loaded Windows runner, far below the
/// patience of a CI job.
const STEP: Duration = Duration::from_secs(60);
/// One whole test, teardown included: the backstop for anything outside a
/// named step, such as a drop while a failed step unwinds.
const TEST: Duration = Duration::from_secs(300);
/// The runtime's shutdown, which otherwise waits forever for a stuck
/// `spawn_blocking` task.
const SHUTDOWN: Duration = Duration::from_secs(30);

tokio::task_local! {
    /// The test's most recent step and whether it finished, for the backstop.
    static LAST_STEP: Arc<Mutex<(String, bool)>>;
}

fn begin_step(name: &str) {
    let _ = LAST_STEP.try_with(|last| {
        if let Ok(mut last) = last.lock() {
            *last = (name.to_string(), false);
        }
    });
}

fn end_step() {
    let _ = LAST_STEP.try_with(|last| {
        if let Ok(mut last) = last.lock() {
            last.1 = true;
        }
    });
}

/// Awaits `work`, failing with the step's name once `limit` passes.
async fn step<T>(name: &str, limit: Duration, work: impl Future<Output = T>) -> T {
    begin_step(name);
    match tokio::time::timeout(limit, work).await {
        Ok(value) => {
            end_step();
            value
        }
        Err(_) => panic!("step `{name}` did not finish within {limit:?}"),
    }
}

/// Runs blocking `work` on the blocking pool under a named [`STEP`] limit,
/// so it can never stall the runtime's own threads. A panic inside `work`
/// propagates unchanged.
async fn blocking<T: Send + 'static>(name: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    match step(name, STEP, tokio::task::spawn_blocking(work)).await {
        Ok(value) => value,
        Err(err) if err.is_panic() => panic::resume_unwind(err.into_panic()),
        Err(err) => panic!("step `{name}` was cancelled: {err}"),
    }
}

/// Runs one test on its own runtime, bounded end to end.
///
/// `#[tokio::test]` drops its runtime when the test ends, and that drop waits
/// forever for a stuck `spawn_blocking` task (the viewer answers every request
/// through one). Here the test runs as a task under [`TEST`], a panic inside
/// it propagates, and the runtime shuts down on a helper thread that is given
/// [`SHUTDOWN`] to finish.
fn run(test: impl Future<Output = ()> + Send + 'static) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build the test runtime");
    let last = Arc::new(Mutex::new((String::from("(none yet)"), true)));
    let task = runtime.spawn(LAST_STEP.scope(last.clone(), test));
    // The timer is created inside the runtime, which it needs.
    let outcome = runtime.block_on(async { tokio::time::timeout(TEST, task).await });
    let (done, shut_down) = mpsc::channel();
    thread::spawn(move || {
        drop(runtime);
        let _ = done.send(());
    });
    let shut_down = shut_down.recv_timeout(SHUTDOWN).is_ok();
    match outcome {
        Ok(Ok(())) => assert!(
            shut_down,
            "step `shut the runtime down` did not finish within {SHUTDOWN:?}: \
             a blocking task is still running"
        ),
        Ok(Err(err)) if err.is_panic() => panic::resume_unwind(err.into_panic()),
        Ok(Err(err)) => panic!("the test task was cancelled: {err}"),
        Err(_) => {
            let (name, finished) = last.lock().map(|last| last.clone()).unwrap_or_default();
            let state = if finished {
                "finished"
            } else {
                "was still running"
            };
            panic!("the test did not finish within {TEST:?}; its last step, `{name}`, {state}")
        }
    }
}

async fn project() -> Project {
    blocking("index the fixture", || {
        Project::indexed(&[
            ("src/greet.ts", ORIGINAL),
            (
                "src/other.ts",
                "import { greet } from './greet';\n\nexport const hi = greet('there');\n",
            ),
        ])
    })
    .await
}

async fn viewer(project: &Project) -> Viewer {
    let root = project.root().to_path_buf();
    blocking("start the viewer", move || Viewer::open(&root, false)).await
}

/// A bounded `GET`.
async fn get(viewer: &Viewer, path: &str) -> Reply {
    step(&format!("GET {path}"), STEP, viewer.get(path)).await
}

/// A bounded `GET` of a 200 JSON answer.
async fn json(viewer: &Viewer, path: &str) -> Value {
    step(&format!("GET {path}"), STEP, viewer.json(path)).await
}

async fn id_of(viewer: &Viewer, name: &str) -> String {
    step(
        &format!("find the id of {name}"),
        STEP,
        viewer.id_of(name, None),
    )
    .await
}

/// Has another writer sync the project, as a daemon's watcher or
/// `codegraph sync` does while the viewer is open.
async fn sync(project: &Project) {
    let root = project.root().to_path_buf();
    blocking("sync the project from another writer", move || {
        codegraph_watch::sync_project_once(&root).expect("sync")
    })
    .await;
}

/// Drops what a test holds off the runtime's threads: a stream or a viewer
/// going away stops the watchers, and stopping one joins its thread.
async fn teardown<T: Send + 'static>(held: T) {
    blocking(
        "tear down the stream, the viewer and the project",
        move || drop(held),
    )
    .await;
}

/// An open event stream.
struct Stream {
    status: StatusCode,
    content_type: Option<String>,
    body: Body,
    buffer: String,
    events: Vec<(String, Value)>,
    comments: usize,
}

impl Stream {
    /// Connects; the server starts its watchers before it answers.
    async fn open(viewer: &Viewer) -> Self {
        let request = Request::builder()
            .uri("/api/events")
            .header("host", format!("127.0.0.1:{PORT}"))
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();
        let response = step("connect to /api/events", STEP, viewer.send(request)).await;
        Self {
            status: response.status(),
            content_type: response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            body: response.into_body(),
            buffer: String::new(),
            events: Vec::new(),
            comments: 0,
        }
    }

    fn ingest(&mut self, chunk: &[u8]) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        while let Some(split) = self.buffer.find("\n\n") {
            let frame: String = self.buffer.drain(..split + 2).collect();
            let (mut name, mut data) = ("message".to_string(), String::new());
            for line in frame.trim_end().split('\n') {
                if line.starts_with(':') {
                    self.comments += 1;
                } else if let Some(rest) = line.strip_prefix("event: ") {
                    name = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data.push_str(rest);
                }
            }
            if !data.is_empty() {
                self.events.push((
                    name,
                    serde_json::from_str(&data).unwrap_or(Value::String(data)),
                ));
            }
        }
    }

    /// The first event of `kind`, waiting at most `timeout` for it.
    async fn wait_for(&mut self, kind: &str, timeout: Duration) -> Value {
        let step_name = format!("await the {kind:?} event");
        begin_step(&step_name);
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((_, data)) = self.events.iter().find(|(name, _)| name == kind) {
                end_step();
                return data.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let seen: Vec<&str> = self.events.iter().map(|(n, _)| n.as_str()).collect();
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.ingest(&data);
                    }
                }
                Ok(other) => {
                    panic!("the stream ended before a {kind:?} event ({other:?}); saw {seen:?}")
                }
                Err(_) => {
                    panic!("step `{step_name}` did not finish within {timeout:?}; saw {seen:?}")
                }
            }
        }
    }

    /// Whether the stream ends within `timeout`.
    async fn ends_within(&mut self, timeout: Duration) -> bool {
        begin_step("await the end of the stream");
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(None) => break,
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.ingest(&data);
                    }
                }
                Ok(Some(Err(_))) => break,
                Err(_) => return false,
            }
        }
        end_step();
        true
    }
}

/// A native watch backend (FSEvents) reports a moment after it is installed;
/// the same allowance the watcher's own tests make.
async fn let_the_watch_settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[test]
fn is_listed_by_the_api_index() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let index = json(&viewer, "/api").await;
        assert!(
            index["endpoints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["path"] == "/api/events")
        );
    });
}

#[test]
fn answers_as_an_event_stream_and_opens_with_the_index_revision() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        let hello = stream.wait_for("hello", WAIT).await;
        assert_eq!(stream.status, StatusCode::OK);
        assert_eq!(
            stream.content_type.as_deref(),
            Some("text/event-stream; charset=utf-8")
        );
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["index"]["files"], 2);
        assert!(hello["index"]["lastIndexedAt"].is_number());
        assert_eq!(hello["heartbeatMs"], 25_000);
        assert!(hello["watching"]["source"].is_boolean());
        assert!(hello["watching"]["index"].is_boolean());
        assert!(hello["degraded"].is_null());
        assert_eq!(viewer.state.events.size(), 1);
        teardown((stream, viewer, project)).await;
    });
}

#[test]
fn never_sends_a_heartbeat_as_an_event() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        stream.wait_for("hello", WAIT).await;
        assert!(
            stream
                .events
                .iter()
                .all(|(name, _)| name != "ping" && name != "message")
        );
        teardown((stream, viewer, project)).await;
    });
}

#[test]
fn answers_head_with_the_stream_headers_and_no_body() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let reply = step(
            "HEAD /api/events",
            STEP,
            viewer.request("HEAD", "/api/events", &[], None),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(
            reply.header("content-type"),
            Some("text/event-stream; charset=utf-8")
        );
        assert!(reply.body.is_empty());
    });
}

#[test]
fn announces_a_source_file_that_changed_on_disk_before_any_sync() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        let hello = stream.wait_for("hello", WAIT).await;
        assert_eq!(
            hello["watching"]["source"], true,
            "the source watcher came up"
        );
        let_the_watch_settle().await;
        project.write(
            "src/greet.ts",
            &format!("{ORIGINAL}\nexport const EXTRA = 1;\n"),
        );
        let changed = stream.wait_for("changed", WAIT).await;
        assert_eq!(changed["type"], "changed");
        let files: Vec<&str> = changed["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            changed["scan"] == true || files.contains(&"src/greet.ts"),
            "{changed}"
        );
        assert!(changed["total"].as_u64().unwrap() as usize >= files.len());
        assert!(files.len() <= 200);
        // ...and the index has NOT moved: this server watches, it never syncs.
        let source = json(&viewer, "/api/source?file=src/greet.ts").await;
        assert_eq!(source["drift"], true);
        teardown((stream, viewer, project)).await;
    });
}

#[test]
fn announces_the_index_moving_and_names_what_the_sync_picked_up() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        let hello = stream.wait_for("hello", WAIT).await;
        assert_eq!(
            hello["watching"]["index"], true,
            "the index watcher came up"
        );
        let_the_watch_settle().await;
        // Another writer re-indexes — what a daemon's watcher or `codegraph sync`
        // does while the viewer is open.
        project.write(
            "src/greet.ts",
            &format!("{ORIGINAL}\nexport const SYNCED = 2;\n"),
        );
        sync(&project).await;
        let moved = stream.wait_for("index", WAIT).await;
        assert_eq!(moved["type"], "index");
        assert_eq!(moved["index"]["files"], 2);
        assert!(
            moved["files"]
                .as_array()
                .unwrap()
                .contains(&Value::from("src/greet.ts")),
            "{moved}"
        );
        assert!(
            moved["total"].as_u64().unwrap() as usize >= moved["files"].as_array().unwrap().len()
        );
        let search = json(&viewer, "/api/search?q=SYNCED").await;
        assert!(
            search["results"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == "SYNCED")
        );
        teardown((stream, viewer, project)).await;
    });
}

#[test]
fn stops_serving_a_symbol_a_sync_in_another_process_deleted() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let old = id_of(&viewer, "shout").await;
        assert_eq!(
            get(&viewer, &format!("/api/node/{}", encode(&old)))
                .await
                .status,
            StatusCode::OK
        );
        project.write("src/greet.ts", &format!("// one\n// two\n{ORIGINAL}"));
        sync(&project).await;
        // Every request opens the index afresh, so a node a sync re-keyed is gone
        // on the very next request.
        assert_eq!(
            get(&viewer, &format!("/api/node/{}", encode(&old)))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let new = id_of(&viewer, "shout").await;
        assert_ne!(new, old);
        let moved = json(&viewer, &format!("/api/node/{}", encode(&new))).await;
        assert_eq!(moved["node"]["line"], 7);
        assert!(moved["counts"]["callees"].as_i64().unwrap() > 0);
    });
}

#[test]
fn closes_every_stream_when_the_api_is_closed() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        stream.wait_for("hello", WAIT).await;
        let state = viewer.state.clone();
        blocking("close the event hub", move || state.events.close()).await;
        assert!(
            stream.ends_within(WAIT).await,
            "the stream ends when the hub closes"
        );
        // A closed hub answers, and does not attach.
        let after = get(&viewer, "/api/events").await;
        assert_eq!(after.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(after.json()["error"], "Shutting down.");
        teardown((stream, viewer, project)).await;
    });
}

#[test]
fn the_last_client_leaving_stops_both_watchers() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let mut stream = Stream::open(&viewer).await;
        stream.wait_for("hello", WAIT).await;
        assert_eq!(viewer.state.events.watching(), (true, true));
        blocking("drop the last client", move || drop(stream)).await;
        assert_eq!(viewer.state.events.size(), 0);
        assert_eq!(viewer.state.events.watching(), (false, false));
        teardown((viewer, project)).await;
    });
}

/* ----------------------------------------------------- /api/source ondrift -- */

#[test]
fn omits_the_slice_by_default_when_the_file_drifted() {
    run(async {
        let project = project().await;
        project.write("src/greet.ts", &format!("// a new first line\n{ORIGINAL}"));
        let viewer = viewer(&project).await;
        let body = json(&viewer, "/api/source?file=src/greet.ts&from=1&to=3").await;
        assert_eq!(body["drift"], true);
        assert_eq!(body["showing"], "none");
        assert!(body.get("lines").is_none());
        assert!(body.get("highlight").is_none());
        assert!(body["reason"].as_str().unwrap().contains("changed on disk"));
    });
}

#[test]
fn serves_the_current_bytes_when_asked_flagged_as_current() {
    run(async {
        let project = project().await;
        let rewritten = format!("// a new first line\n{ORIGINAL}");
        project.write("src/greet.ts", &rewritten);
        let viewer = viewer(&project).await;
        let body = json(
            &viewer,
            "/api/source?file=src/greet.ts&from=1&ondrift=current",
        )
        .await;
        assert_eq!(body["drift"], true);
        assert_eq!(body["showing"], "current");
        assert_eq!(body["lines"][0], "// a new first line");
        assert_eq!(
            body["totalLines"],
            rewritten.trim_end_matches('\n').split('\n').count()
        );
        assert_eq!(
            body["highlight"]["lines"].as_array().unwrap().len(),
            body["lines"].as_array().unwrap().len()
        );
        assert!(body["reason"].as_str().unwrap().contains("current lines"));
    });
}

#[test]
fn says_showing_indexed_when_there_is_no_drift_with_or_without_the_flag() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let plain = json(&viewer, "/api/source?file=src/greet.ts&from=1&to=2").await;
        assert_eq!(plain["drift"], false);
        assert_eq!(plain["showing"], "indexed");
        let asked = json(
            &viewer,
            "/api/source?file=src/greet.ts&from=1&to=2&ondrift=current",
        )
        .await;
        assert_eq!(asked["showing"], "indexed");
        assert_eq!(asked["lines"], plain["lines"]);
    });
}

#[test]
fn rejects_an_ondrift_value_it_does_not_implement() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let reply = get(&viewer, "/api/source?file=src/greet.ts&ondrift=guess").await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            reply.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(reply.json()["code"], "bad-request");
    });
}

#[test]
fn answers_an_empty_slice_rather_than_a_400_when_a_drifted_file_shrank() {
    run(async {
        let project = project().await;
        project.write("src/greet.ts", "export const only = 1;\n");
        let viewer = viewer(&project).await;
        let reply = get(
            &viewer,
            "/api/source?file=src/greet.ts&from=5&to=9&ondrift=current",
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        let body = reply.json();
        assert_eq!(body["showing"], "current");
        assert_eq!(body["lines"], serde_json::json!([]));
        assert_eq!(body["totalLines"], 1);
    });
}

#[test]
fn still_refuses_a_path_outside_the_project_ondrift_or_not() {
    run(async {
        let project = project().await;
        let viewer = viewer(&project).await;
        let reply = get(&viewer, "/api/source?file=/etc/passwd&ondrift=current").await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(reply.json()["code"], "refused");
    });
}
