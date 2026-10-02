//! A project that IS routed — the `/api/routes` half of upstream
//! `__tests__/ui-server-api.test.ts` (`v1.6.1`). Upstream's fixture is an
//! Express app; the Rust port has no Express resolver, so the same four routes
//! are declared the way a resolver here mints them: a NestJS controller.

mod support;

use axum::http::StatusCode;
use support::{Project, encode};

const PACKAGE_JSON: &str = r#"{ "dependencies": { "@nestjs/common": "^10.0.0" } }"#;

const CONTROLLER: &str = "import { Controller, Get, Post, Delete } from '@nestjs/common';

@Controller('users')
export class UsersController {
  @Get()
  listUsers() {
    return [];
  }

  @Get(':id')
  getUser() {
    return {};
  }

  @Post()
  createUser() {
    return {};
  }

  @Delete(':id')
  deleteUser() {
    return {};
  }
}
";

fn routed() -> Project {
    Project::indexed(&[
        ("package.json", PACKAGE_JSON),
        ("src/users.controller.ts", CONTROLLER),
    ])
}

fn urls(entries: &serde_json::Value) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["url"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn maps_each_url_to_its_handler_with_a_node_id_to_navigate_to() {
    let project = routed();
    let viewer = project.viewer();
    let body = viewer.json("/api/routes").await;
    assert_eq!(body["routed"], true);
    assert_eq!(body["routeCount"], 4);
    assert_eq!(body["shown"], 4);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["topHandlerFile"], "src/users.controller.ts");
    assert_eq!(body["topHandlerFileCount"], 4);
    let urls = urls(&body["entries"]);
    for url in [
        "GET /users",
        "GET /users/:id",
        "POST /users",
        "DELETE /users/:id",
    ] {
        assert!(urls.iter().any(|u| u == url), "{url} in {urls:?}");
    }
    let list = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["url"] == "GET /users")
        .unwrap()
        .clone();
    assert_eq!(list["method"], "GET");
    assert_eq!(list["path"], "/users");
    assert_eq!(list["handler"], "listUsers");
    assert_eq!(list["handlerKind"], "method");
    assert_eq!(list["file"], "src/users.controller.ts");
    assert!(list["line"].as_i64().unwrap() > 0);
    // The manifest carries no ids of its own; resolving them is what makes a
    // route row clickable, so it has to actually resolve.
    let handler = viewer
        .json(&format!(
            "/api/node/{}",
            encode(list["handlerId"].as_str().unwrap())
        ))
        .await;
    assert_eq!(handler["node"]["name"], "listUsers");
}

#[tokio::test]
async fn offers_its_routes_as_entry_points_ahead_of_anything_derived() {
    let project = routed();
    let body = project.viewer().json("/api/entrypoints").await;
    assert_eq!(body["routes"]["routed"], true);
    assert_eq!(body["routes"]["routeCount"], 4);
    assert_eq!(body["frameworks"], serde_json::json!(["nestjs"]));
    let urls = urls(&body["routes"]["items"]["items"]);
    for url in [
        "GET /users",
        "GET /users/:id",
        "POST /users",
        "DELETE /users/:id",
    ] {
        assert!(urls.iter().any(|u| u == url), "{url}");
    }
    assert!(
        body["routes"]["items"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["handlerId"].is_string())
    );
}

#[tokio::test]
async fn honours_the_limit_and_says_when_it_cut_the_list() {
    let project = routed();
    let body = project.viewer().json("/api/routes?limit=3").await;
    assert_eq!(body["routed"], true);
    assert_eq!(body["entries"].as_array().unwrap().len(), 3);
    assert_eq!(body["shown"], 3);
    assert_eq!(body["truncated"], true);
    // The headline count is the whole graph's, not the page's.
    assert_eq!(body["routeCount"], 4);
}

#[tokio::test]
async fn links_every_route_to_its_handler_however_many_files_the_handlers_live_in_1975() {
    // 70 handlers, one file each: a lookup that stopped at the 60th file would
    // report the rest as "not in the index".
    let mut files: Vec<(String, String)> = vec![("package.json".into(), PACKAGE_JSON.into())];
    for n in 1..=70 {
        files.push((
            format!("src/h{n}.controller.ts"),
            format!(
                "import {{ Controller, Get }} from '@nestjs/common';\n\n@Controller('r{n}')\nexport class H{n}Controller {{\n  @Get()\n  h{n}() {{\n    return {n};\n  }}\n}}\n"
            ),
        ));
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let project = Project::indexed(&borrowed);
    let body = project.viewer().json("/api/routes?limit=200").await;
    assert_eq!(body["entries"].as_array().unwrap().len(), 70);
    let unlinked: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["handlerId"].is_null())
        .map(|e| e["url"].as_str().unwrap())
        .collect();
    assert!(unlinked.is_empty(), "{unlinked:?}");
}

#[tokio::test]
async fn a_test_suites_routes_are_not_a_routed_app() {
    // Handlers that live in a test suite are dropped from the manifest, and
    // fewer than three production routes is not a routed app.
    let suite = CONTROLLER.replace("UsersController", "UsersSpecController");
    let project = Project::indexed(&[
        ("package.json", PACKAGE_JSON),
        ("test/users.controller.ts", &suite),
    ]);
    let reply = project.viewer().get("/api/routes").await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["routed"], false);
    assert_eq!(
        body["routeCount"], 4,
        "the routes exist; they are just not the product's"
    );
}
