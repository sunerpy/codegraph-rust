//! The live channel and `?ondrift=` — a port of upstream
//! `__tests__/ui-events-api.test.ts` (`v1.6.1`). The stream is read frame by
//! frame off the router's streaming body; every wait is bounded and fails with
//! what it saw.

mod support;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use support::{PORT, Project, Viewer, encode};

const ORIGINAL: &str = "export function greet(name: string): string {
  return 'hello ' + name;
}

export function shout(name: string): string {
  return greet(name).toUpperCase();
}
";

fn project() -> Project {
    Project::indexed(&[
        ("src/greet.ts", ORIGINAL),
        (
            "src/other.ts",
            "import { greet } from './greet';\n\nexport const hi = greet('there');\n",
        ),
    ])
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
    async fn open(viewer: &Viewer) -> Self {
        let request = Request::builder()
            .uri("/api/events")
            .header("host", format!("127.0.0.1:{PORT}"))
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();
        let response = viewer.send(request).await;
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
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((_, data)) = self.events.iter().find(|(name, _)| name == kind) {
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
                Err(_) => panic!("no {kind:?} event within {timeout:?}; saw {seen:?}"),
            }
        }
    }

    /// Whether the stream ends within `timeout`.
    async fn ends_within(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(None) => return true,
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.ingest(&data);
                    }
                }
                Ok(Some(Err(_))) => return true,
                Err(_) => return false,
            }
        }
    }
}

const WAIT: Duration = Duration::from_secs(15);

/// A native watch backend (FSEvents) reports a moment after it is installed;
/// the same allowance the watcher's own tests make.
async fn let_the_watch_settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn is_listed_by_the_api_index() {
    let project = project();
    let index = project.viewer().json("/api").await;
    assert!(
        index["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "/api/events")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_as_an_event_stream_and_opens_with_the_index_revision() {
    let project = project();
    let viewer = project.viewer();
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
}

#[tokio::test(flavor = "multi_thread")]
async fn never_sends_a_heartbeat_as_an_event() {
    let project = project();
    let viewer = project.viewer();
    let mut stream = Stream::open(&viewer).await;
    stream.wait_for("hello", WAIT).await;
    assert!(
        stream
            .events
            .iter()
            .all(|(name, _)| name != "ping" && name != "message")
    );
}

#[tokio::test]
async fn answers_head_with_the_stream_headers_and_no_body() {
    let project = project();
    let reply = project
        .viewer()
        .request("HEAD", "/api/events", &[], None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("text/event-stream; charset=utf-8")
    );
    assert!(reply.body.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn announces_a_source_file_that_changed_on_disk_before_any_sync() {
    let project = project();
    let viewer = project.viewer();
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
    let source = viewer.json("/api/source?file=src/greet.ts").await;
    assert_eq!(source["drift"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn announces_the_index_moving_and_names_what_the_sync_picked_up() {
    let project = project();
    let viewer = project.viewer();
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
    let root = project.root().to_path_buf();
    tokio::task::spawn_blocking(move || codegraph_watch::sync_project_once(&root).expect("sync"))
        .await
        .unwrap();
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
    assert!(moved["total"].as_u64().unwrap() as usize >= moved["files"].as_array().unwrap().len());
    let search = viewer.json("/api/search?q=SYNCED").await;
    assert!(
        search["results"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "SYNCED")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_serving_a_symbol_a_sync_in_another_process_deleted() {
    let project = project();
    let viewer = project.viewer();
    let old = viewer.id_of("shout", None).await;
    assert_eq!(
        viewer
            .get(&format!("/api/node/{}", encode(&old)))
            .await
            .status,
        StatusCode::OK
    );
    project.write("src/greet.ts", &format!("// one\n// two\n{ORIGINAL}"));
    let root = project.root().to_path_buf();
    tokio::task::spawn_blocking(move || codegraph_watch::sync_project_once(&root).expect("sync"))
        .await
        .unwrap();
    // Every request opens the index afresh, so a node a sync re-keyed is gone
    // on the very next request.
    assert_eq!(
        viewer
            .get(&format!("/api/node/{}", encode(&old)))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let new = viewer.id_of("shout", None).await;
    assert_ne!(new, old);
    let moved = viewer.json(&format!("/api/node/{}", encode(&new))).await;
    assert_eq!(moved["node"]["line"], 7);
    assert!(moved["counts"]["callees"].as_i64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn closes_every_stream_when_the_api_is_closed() {
    let project = project();
    let viewer = project.viewer();
    let mut stream = Stream::open(&viewer).await;
    stream.wait_for("hello", WAIT).await;
    viewer.state.events.close();
    assert!(
        stream.ends_within(WAIT).await,
        "the stream ends when the hub closes"
    );
    // A closed hub answers, and does not attach.
    let after = viewer.get("/api/events").await;
    assert_eq!(after.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(after.json()["error"], "Shutting down.");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_client_leaving_stops_both_watchers() {
    let project = project();
    let viewer = project.viewer();
    let mut stream = Stream::open(&viewer).await;
    stream.wait_for("hello", WAIT).await;
    assert_eq!(viewer.state.events.watching(), (true, true));
    drop(stream);
    assert_eq!(viewer.state.events.size(), 0);
    assert_eq!(viewer.state.events.watching(), (false, false));
}

/* ----------------------------------------------------- /api/source ondrift -- */

#[tokio::test]
async fn omits_the_slice_by_default_when_the_file_drifted() {
    let project = project();
    project.write("src/greet.ts", &format!("// a new first line\n{ORIGINAL}"));
    let body = project
        .viewer()
        .json("/api/source?file=src/greet.ts&from=1&to=3")
        .await;
    assert_eq!(body["drift"], true);
    assert_eq!(body["showing"], "none");
    assert!(body.get("lines").is_none());
    assert!(body.get("highlight").is_none());
    assert!(body["reason"].as_str().unwrap().contains("changed on disk"));
}

#[tokio::test]
async fn serves_the_current_bytes_when_asked_flagged_as_current() {
    let project = project();
    let rewritten = format!("// a new first line\n{ORIGINAL}");
    project.write("src/greet.ts", &rewritten);
    let body = project
        .viewer()
        .json("/api/source?file=src/greet.ts&from=1&ondrift=current")
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
}

#[tokio::test]
async fn says_showing_indexed_when_there_is_no_drift_with_or_without_the_flag() {
    let project = project();
    let viewer = project.viewer();
    let plain = viewer
        .json("/api/source?file=src/greet.ts&from=1&to=2")
        .await;
    assert_eq!(plain["drift"], false);
    assert_eq!(plain["showing"], "indexed");
    let asked = viewer
        .json("/api/source?file=src/greet.ts&from=1&to=2&ondrift=current")
        .await;
    assert_eq!(asked["showing"], "indexed");
    assert_eq!(asked["lines"], plain["lines"]);
}

#[tokio::test]
async fn rejects_an_ondrift_value_it_does_not_implement() {
    let project = project();
    let reply = project
        .viewer()
        .get("/api/source?file=src/greet.ts&ondrift=guess")
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(reply.json()["code"], "bad-request");
}

#[tokio::test]
async fn answers_an_empty_slice_rather_than_a_400_when_a_drifted_file_shrank() {
    let project = project();
    project.write("src/greet.ts", "export const only = 1;\n");
    let reply = project
        .viewer()
        .get("/api/source?file=src/greet.ts&from=5&to=9&ondrift=current")
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["showing"], "current");
    assert_eq!(body["lines"], serde_json::json!([]));
    assert_eq!(body["totalLines"], 1);
}

#[tokio::test]
async fn still_refuses_a_path_outside_the_project_ondrift_or_not() {
    let project = project();
    let reply = project
        .viewer()
        .get("/api/source?file=/etc/passwd&ondrift=current")
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["code"], "refused");
}
