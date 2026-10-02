//! Saved trails — the viewer's one write — a port of upstream
//! `__tests__/ui-trails.test.ts` (`v1.6.1`). Upstream's cases share one server
//! and run in order; here each test owns its project, so a case that depends
//! on an earlier one replays it.

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{Project, Viewer};

const HANDLER_TS: &str = "import { load } from './service';

export function handleRequest(key: string): string {
  return load(key);
}
";

const SERVICE_TS: &str = "import { read } from './cache';

export function load(key: string): string {
  return read(key);
}

export function retired(): string {
  return 'nothing calls me after the edit';
}
";

const CACHE_TS: &str = "export function read(key: string): string {
  return key;
}
";

fn project() -> Project {
    Project::indexed(&[
        ("src/handler.ts", HANDLER_TS),
        ("src/service.ts", SERVICE_TS),
        ("src/cache.ts", CACHE_TS),
    ])
}

fn trails_dir(project: &Project) -> std::path::PathBuf {
    project.root().join(".codegraph/ui/trails")
}

async fn walk(viewer: &Viewer) -> Value {
    json!([
        { "dir": "start", "id": viewer.id_of("handleRequest", None).await },
        { "dir": "down", "id": viewer.id_of("load", None).await },
        { "dir": "down", "id": viewer.id_of("read", None).await },
    ])
}

async fn save(viewer: &Viewer, body: Value) -> support::Reply {
    viewer.write("POST", "/api/trails", Some(&body)).await
}

#[tokio::test]
async fn is_an_empty_list_not_an_error_before_anything_is_saved() {
    let project = project();
    let viewer = project.viewer();
    let body = viewer.json("/api/trails").await;
    assert_eq!(body["trails"], json!([]));
    assert_eq!(body["readOnly"], false);
    assert_eq!(body["directory"], ".codegraph/ui/trails");
    let index = viewer.json("/api").await;
    assert!(
        index["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "/api/trails")
    );
    assert!(
        index["writes"]
            .as_array()
            .unwrap()
            .contains(&Value::from("POST /api/trails"))
    );
}

#[tokio::test]
async fn saves_the_walk_and_answers_with_the_whole_list() {
    let project = project();
    let viewer = project.viewer();
    let hops = walk(&viewer).await;
    let reply = save(
        &viewer,
        json!({ "name": "How a request is served", "note": "the whole path", "hops": hops }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["saved"], "how-a-request-is-served");
    assert_eq!(body["replaced"], false);
    assert_eq!(body["trails"].as_array().unwrap().len(), 1);
    let trail = &body["trails"][0];
    assert_eq!(trail["name"], "How a request is served");
    assert_eq!(trail["note"], "the whole path");
    assert_eq!(trail["intact"], true);
    assert_eq!(trail["resolved"], 3);
    assert_eq!(trail["openCount"], 3);
    let names: Vec<&str> = trail["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["handleRequest", "load", "read"]);
    assert_eq!(trail["hops"][1]["qualifiedName"], "load");
    assert_eq!(trail["hops"][1]["savedFile"], "src/service.ts");

    // One readable JSON file, and nothing else: no temp file survives the rename.
    let file = trails_dir(&project).join("how-a-request-is-served.json");
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(raw["version"], 1);
    assert_eq!(raw["hops"].as_array().unwrap().len(), 3);
    assert_eq!(raw["hops"][0]["qualifiedName"], "handleRequest");
    assert!(raw["createdAt"].is_string());
    let listing: Vec<String> = std::fs::read_dir(trails_dir(&project))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listing, ["how-a-request-is-served.json"]);
}

#[tokio::test]
async fn replaces_a_trail_saved_under_the_same_name_keeping_its_created_at() {
    let project = project();
    let viewer = project.viewer();
    let hops = walk(&viewer).await;
    let first = save(
        &viewer,
        json!({ "name": "How a request is served", "note": "n", "hops": hops }),
    )
    .await
    .json();
    let before = first["trails"][0].clone();
    let only = json!([{ "dir": "start", "id": viewer.id_of("handleRequest", None).await }]);
    let body = save(
        &viewer,
        json!({ "name": "How a request is served", "hops": only }),
    )
    .await
    .json();
    assert_eq!(body["replaced"], true);
    assert_eq!(body["trails"].as_array().unwrap().len(), 1);
    assert_eq!(body["trails"][0]["createdAt"], before["createdAt"]);
    assert_eq!(body["trails"][0]["hops"].as_array().unwrap().len(), 1);
    assert_eq!(body["trails"][0]["note"], "");
}

#[tokio::test]
async fn gives_a_different_name_its_own_file_rather_than_colliding() {
    let project = project();
    let viewer = project.viewer();
    let hops = walk(&viewer).await;
    save(
        &viewer,
        json!({ "name": "How a request is served", "hops": hops }),
    )
    .await;
    let load = json!([{ "dir": "start", "id": viewer.id_of("load", None).await }]);
    let body = save(
        &viewer,
        json!({ "name": "How a request is served!", "hops": load }),
    )
    .await
    .json();
    assert_eq!(body["saved"], "how-a-request-is-served-2");
    assert_eq!(body["trails"].as_array().unwrap().len(), 2);

    // DELETE: removes the file and answers with the list that is left.
    let deleted = viewer
        .write("DELETE", "/api/trails/how-a-request-is-served-2", None)
        .await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(deleted.json()["deleted"], "how-a-request-is-served-2");
    assert_eq!(deleted.json()["trails"].as_array().unwrap().len(), 1);
    assert!(
        !trails_dir(&project)
            .join("how-a-request-is-served-2.json")
            .exists()
    );
}

#[tokio::test]
async fn refuses_a_hop_the_index_does_not_hold() {
    let project = project();
    let reply = save(
        &project.viewer(),
        json!({ "name": "invented", "hops": [{ "dir": "start", "id": "function:not-a-real-id" }] }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("Hop 1 is not in the index")
    );
}

#[tokio::test]
async fn refuses_a_nameless_or_hopless_trail() {
    let project = project();
    let viewer = project.viewer();
    let no_name = save(&viewer, json!({ "name": "  ", "hops": [] })).await;
    assert_eq!(no_name.status, StatusCode::BAD_REQUEST);
    let no_hops = save(&viewer, json!({ "name": "x", "hops": [] })).await;
    assert_eq!(no_hops.status, StatusCode::BAD_REQUEST);
    assert!(
        no_hops.json()["error"]
            .as_str()
            .unwrap()
            .contains("at least one hop")
    );
    let empty = viewer
        .request("POST", "/api/trails", &[("x-codegraph-ui", "1")], None)
        .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert!(
        empty.json()["error"]
            .as_str()
            .unwrap()
            .contains("needs a JSON body")
    );
    let broken = viewer
        .request(
            "POST",
            "/api/trails",
            &[
                ("x-codegraph-ui", "1"),
                ("content-type", "application/json"),
            ],
            Some(b"{not json"),
        )
        .await;
    assert_eq!(broken.status, StatusCode::BAD_REQUEST);
    assert!(
        broken.json()["error"]
            .as_str()
            .unwrap()
            .contains("not valid JSON")
    );
    let huge = vec![b' '; 64 * 1024 + 1];
    let oversized = viewer
        .request(
            "POST",
            "/api/trails",
            &[
                ("x-codegraph-ui", "1"),
                ("content-type", "application/json"),
            ],
            Some(&huge),
        )
        .await;
    assert_eq!(oversized.status, StatusCode::BAD_REQUEST);
    assert!(
        oversized.json()["error"]
            .as_str()
            .unwrap()
            .contains("too large")
    );
}

#[tokio::test]
async fn is_a_404_for_a_trail_that_is_not_there() {
    let project = project();
    let reply = project
        .viewer()
        .write("DELETE", "/api/trails/never-existed", None)
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refuses_an_id_shaped_like_a_path_before_it_is_joined_to_anything() {
    let project = project();
    let viewer = project.viewer();
    let reply = viewer
        .write("DELETE", "/api/trails/..%2f..%2fetc%2fpasswd", None)
        .await;
    assert!([StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND].contains(&reply.status));
    assert!(
        reply
            .header("content-type")
            .unwrap()
            .contains("application/json")
    );
    let shaped = viewer.write("DELETE", "/api/trails/Not.A.Slug", None).await;
    assert_eq!(shaped.status, StatusCode::BAD_REQUEST);
    assert!(
        shaped.json()["error"]
            .as_str()
            .unwrap()
            .contains("is not a saved trail id")
    );
    let bare = viewer.write("DELETE", "/api/trails", None).await;
    assert_eq!(bare.status, StatusCode::BAD_REQUEST);
    assert!(
        bare.json()["error"]
            .as_str()
            .unwrap()
            .contains("needs its id")
    );
}

/* ------------------------------------------------------ the write boundary -- */

#[tokio::test]
async fn refuses_a_post_without_the_marker_header() {
    let project = project();
    let body = json!({ "name": "forged", "hops": [] }).to_string();
    let reply = project
        .viewer()
        .request(
            "POST",
            "/api/trails",
            &[("content-type", "application/json")],
            Some(body.as_bytes()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["code"], "refused");
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("x-codegraph-ui")
    );
}

#[tokio::test]
async fn refuses_a_post_whose_body_claims_to_be_a_form() {
    let project = project();
    let body = json!({ "name": "forged", "hops": [] }).to_string();
    let reply = project
        .viewer()
        .request(
            "POST",
            "/api/trails",
            &[
                ("x-codegraph-ui", "1"),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            Some(body.as_bytes()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("application/json")
    );
}

#[tokio::test]
async fn refuses_a_post_from_a_foreign_origin_even_with_the_marker() {
    let project = project();
    let body = json!({ "name": "forged", "hops": [] }).to_string();
    let reply = project
        .viewer()
        .request(
            "POST",
            "/api/trails",
            &[
                ("x-codegraph-ui", "1"),
                ("content-type", "application/json"),
                ("origin", "https://evil.example"),
            ],
            Some(body.as_bytes()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refuses_a_write_anywhere_but_api_and_still_serves_the_asset_on_get() {
    let project = project();
    let viewer = project.viewer();
    let post = viewer
        .write("POST", "/index.html", Some(&json!({ "a": 1 })))
        .await;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(post.header("allow"), Some("GET, HEAD"));
    assert_eq!(viewer.get("/index.html").await.status, StatusCode::OK);
    let other = viewer.write("POST", "/api/stats", Some(&json!({}))).await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
    assert_eq!(other.header("allow"), Some("GET, HEAD"));
}

#[tokio::test]
async fn still_refuses_a_method_it_has_never_answered() {
    let project = project();
    let reply = project
        .viewer()
        .request("PUT", "/api/trails", &[], None)
        .await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn refuses_every_write_under_read_only_but_still_lists_what_is_there() {
    let project = project();
    let writer = project.viewer();
    let hops = walk(&writer).await;
    save(
        &writer,
        json!({ "name": "How a request is served", "hops": hops }),
    )
    .await;

    let viewer = project.read_only_viewer();
    let list = viewer.json("/api/trails").await;
    assert_eq!(list["readOnly"], true);
    assert!(
        list["readOnlyReason"]
            .as_str()
            .unwrap()
            .contains("--read-only")
    );
    assert!(!list["trails"].as_array().unwrap().is_empty());
    let save_reply = save(
        &viewer,
        json!({ "name": "nope", "hops": [{ "dir": "start", "id": "x" }] }),
    )
    .await;
    assert_eq!(save_reply.status, StatusCode::FORBIDDEN);
    assert_eq!(save_reply.json()["code"], "refused");
    let remove = viewer
        .write("DELETE", "/api/trails/how-a-request-is-served", None)
        .await;
    assert_eq!(remove.status, StatusCode::FORBIDDEN);
    assert!(
        trails_dir(&project)
            .join("how-a-request-is-served.json")
            .exists()
    );
}

/* ----------------------------------------------- surviving a re-index ---- */

#[tokio::test]
async fn re_resolves_hops_by_qualified_name_once_every_node_id_has_changed() {
    let project = project();
    let viewer = project.viewer();
    let saved = save(
        &viewer,
        json!({
            "name": "The whole walk",
            "hops": [
                { "dir": "start", "id": viewer.id_of("handleRequest", None).await },
                { "dir": "down", "id": viewer.id_of("load", None).await },
                { "dir": "down", "id": viewer.id_of("read", None).await },
                { "dir": "down", "id": viewer.id_of("retired", None).await },
            ],
        }),
    )
    .await
    .json();
    let before = saved["trails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "the-whole-walk")
        .unwrap()
        .clone();
    assert_eq!(before["intact"], true);
    let ids_before: Vec<String> = before["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect();

    // Move the world underneath it: `handleRequest` shifts down its file (its
    // id contains its line), `read` moves to another file, `retired` goes.
    project.write(
        "src/handler.ts",
        "import { load } from './service';

// A comment inserted above the symbol. This alone renames it.
// Another line.
// And another.

export function handleRequest(key: string): string {
  return load(key);
}
",
    );
    project.write(
        "src/service.ts",
        "import { read } from './store';

export function load(key: string): string {
  return read(key);
}
",
    );
    project.write("src/cache.ts", "export const unused = 1;\n");
    project.write("src/store.ts", CACHE_TS);
    project.index();

    let list = viewer.json("/api/trails").await;
    let after = list["trails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "the-whole-walk")
        .unwrap()
        .clone();
    let hops = after["hops"].as_array().unwrap();
    assert_ne!(hops[0]["id"].as_str(), Some(ids_before[0].as_str()));
    assert!(hops[0]["id"].is_string());

    assert_eq!(hops[0]["status"], "ok");
    assert_eq!(hops[0]["file"], "src/handler.ts");
    assert!(hops[0]["line"].as_i64().unwrap() > hops[0]["savedLine"].as_i64().unwrap());
    assert_eq!(hops[1]["status"], "ok");
    assert_eq!(hops[2]["status"], "moved");
    assert_eq!(hops[2]["savedFile"], "src/cache.ts");
    assert_eq!(hops[2]["file"], "src/store.ts");
    assert!(hops[2]["note"].as_str().unwrap().contains("src/cache.ts"));
    assert!(hops[2]["note"].as_str().unwrap().contains("src/store.ts"));
    assert_eq!(hops[3]["status"], "missing");
    assert!(hops[3]["id"].is_null());
    assert!(
        hops[3]["note"]
            .as_str()
            .unwrap()
            .contains("moved or renamed")
    );

    assert_eq!(after["intact"], false);
    assert_eq!(after["resolved"], 3);
    assert_eq!(after["openFrom"], 1);
    assert_eq!(after["openCount"], 3);
    assert_eq!(after["openId"], hops[2]["id"]);
    let encoded = after["encoded"].as_str().unwrap();
    assert_eq!(encoded.split(',').count(), 3);
    assert!(encoded.starts_with('s'));
}
