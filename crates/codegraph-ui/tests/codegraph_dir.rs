//! A `CODEGRAPH_DIR` override moves everything the viewer touches with the
//! index: the database it reads, the directory it watches for the index
//! moving, and the trails it writes. Its own test binary, because the variable
//! is process-global.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use support::{PORT, Project};

const OVERRIDE: &str = ".cg-alt";

#[tokio::test(flavor = "multi_thread")]
async fn trails_and_the_index_watch_follow_the_index_root() {
    // SAFETY: the only test in this binary, set before any thread reads it.
    unsafe { std::env::set_var("CODEGRAPH_DIR", OVERRIDE) };
    let project = Project::indexed(&[
        (
            "src/a.ts",
            "import { b } from './b';\n\nexport function a(): string {\n  return b();\n}\n",
        ),
        (
            "src/b.ts",
            "export function b(): string {\n  return 'b';\n}\n",
        ),
    ]);
    assert!(project.root().join(OVERRIDE).join("codegraph.db").is_file());
    assert!(
        !project.root().join(".codegraph").exists(),
        "nothing lands in the default root"
    );

    let viewer = project.viewer();
    assert_eq!(
        viewer.state.paths.current_root(),
        project.root().join(OVERRIDE)
    );
    let stats = viewer.json("/api/stats").await;
    assert_eq!(stats["graph"]["files"], 2);

    // Trails live under the index root it resolved.
    let list = viewer.json("/api/trails").await;
    assert_eq!(list["directory"], format!("{OVERRIDE}/ui/trails"));
    let id = viewer.id_of("a", Some("function")).await;
    let saved = viewer
        .write(
            "POST",
            "/api/trails",
            Some(&json!({ "name": "Tour", "hops": [{ "dir": "start", "id": id }] })),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    assert!(
        project
            .root()
            .join(OVERRIDE)
            .join("ui/trails/tour.json")
            .is_file()
    );
    assert!(!project.root().join(".codegraph").exists());

    // The index half of the live channel watches the override, too.
    let request = Request::builder()
        .uri("/api/events")
        .header("host", format!("127.0.0.1:{PORT}"))
        .body(Body::empty())
        .unwrap();
    let mut body = viewer.send(request).await.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(15), body.frame())
        .await
        .expect("a hello frame")
        .expect("a frame")
        .expect("data");
    let text = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
    assert!(text.contains("event: hello"), "{text}");
    assert!(text.contains(r#""index":true"#), "{text}");
}
