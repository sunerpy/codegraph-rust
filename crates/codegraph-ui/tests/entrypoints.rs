//! `GET /api/entrypoints` — the server half of upstream
//! `__tests__/ui-entrypoints-api.test.ts` (`v1.6.1`). Its panel-model cases
//! (`buildEntryPanel`, `frameworkPhrase`) run against literal payloads in
//! `ui/tests/`; its routed case uses a Go fixture whose routes come from a
//! resolver the Rust port does not have, so the routed half here is the NestJS
//! controller every resolver-backed route test in this crate uses.

mod support;

use axum::http::StatusCode;
use serde_json::Value;
use support::Project;

fn library() -> Project {
    Project::indexed(&[
        (
            "src/store.ts",
            "export function insertNode(name: string): string {
  return name.trim();
}

export function readNode(name: string): string {
  return insertNode(name);
}
",
        ),
        // Module-level statements: the only reason an executable root is visible.
        (
            "src/main.ts",
            "import { insertNode, readNode } from './store';

const first = insertNode('boot');
const second = readNode('warm');

export const started = [first, second];
",
        ),
        (
            "__tests__/store.test.ts",
            "import { insertNode } from '../src/store';

export function exercisesTheStore(): string {
  return insertNode('x');
}

exercisesTheStore();
",
        ),
        // A fixture is not a test, even though the ranking treats it as one.
        ("__tests__/fixtures/sample.ts", "export const sample = 1;\n"),
    ])
}

fn routed() -> Project {
    Project::indexed(&[
        (
            "package.json",
            r#"{ "dependencies": { "@nestjs/common": "^10.0.0" } }"#,
        ),
        (
            "src/payroll.controller.ts",
            "import { Controller, Get, Post } from '@nestjs/common';
import { runPayrollCycleAll } from './payroll.service';

@Controller('v1/payroll/cycles')
export class PayrollController {
  @Post(':cycleID/run')
  runCycle() {
    return runPayrollCycleAll();
  }

  @Get(':cycleID')
  getCycle() {
    return {};
  }

  @Get(':cycleID/payslips')
  listPayslips() {
    return [];
  }
}
",
        ),
        (
            "src/payroll.service.ts",
            "import { upsert } from './store';

export function runPayrollCycleAll(): string {
  return upsert('cycle');
}
",
        ),
        (
            "src/store.ts",
            "export function upsert(row: string): string {\n  return row;\n}\n",
        ),
    ])
}

fn files_of(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap().to_string())
        .collect()
}

/* --------------------------------------------- entry points, routed ----- */

#[tokio::test]
async fn names_the_framework_the_route_list_came_from() {
    let project = routed();
    let body = project.viewer().json("/api/entrypoints").await;
    assert!(
        body["frameworks"]
            .as_array()
            .unwrap()
            .contains(&Value::from("nestjs"))
    );
}

#[tokio::test]
async fn lists_every_route_with_the_symbol_that_serves_it() {
    let project = routed();
    let body = project.viewer().json("/api/entrypoints").await;
    assert_eq!(body["routes"]["routed"], true);
    assert_eq!(body["routes"]["routeCount"], 3);
    let rows = body["routes"]["items"]["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let run = rows
        .iter()
        .find(|r| r["method"] == "POST")
        .expect("the POST route");
    assert_eq!(run["path"], "/v1/payroll/cycles/:cycleID/run");
    assert_eq!(run["handler"], "runCycle");
    assert_eq!(run["file"], "src/payroll.controller.ts");
    assert!(rows.iter().all(|r| r["handlerId"].is_string()));
    assert!(rows.iter().all(|r| r["routeLine"].as_i64().unwrap() > 0));
}

#[tokio::test]
async fn draws_the_flow_from_a_route_handler_down_to_the_store() {
    let project = routed();
    let flow = project
        .viewer()
        .json("/api/flow?from=runCycle&to=upsert")
        .await;
    let hops: Vec<&str> = flow["flows"][0]["hops"]
        .as_array()
        .unwrap_or_else(|| panic!("{flow}"))
        .iter()
        .map(|h| h["node"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(hops.first(), Some(&"runCycle"));
    assert_eq!(hops.last(), Some(&"upsert"));
    assert!(hops.contains(&"runPayrollCycleAll"));
    assert!(
        flow["flows"][0]["hops"].as_array().unwrap()[1..]
            .iter()
            .all(|h| !h["edge"].is_null())
    );
}

#[tokio::test]
async fn answers_a_second_time_from_the_cache() {
    let project = routed();
    let viewer = project.viewer();
    let first = viewer.json("/api/entrypoints").await;
    let again = viewer.json("/api/entrypoints").await;
    assert_eq!(first["timing"]["cached"], false);
    assert_eq!(again["timing"]["cached"], true);
    assert_eq!(
        again["routes"]["items"]["items"],
        first["routes"]["items"]["items"]
    );
}

#[tokio::test]
async fn refuses_a_route_window_it_cannot_answer_truthfully() {
    let project = routed();
    let reply = project.viewer().get("/api/entrypoints?routes=2").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.json()["error"].as_str().unwrap().contains("routes"));
}

/* ----------------------------------------- entry points, no routes ------ */

#[tokio::test]
async fn says_it_is_not_a_routed_app_instead_of_drawing_an_empty_list() {
    let project = library();
    let body = project.viewer().json("/api/entrypoints").await;
    assert_eq!(body["routes"]["routed"], false);
    assert_eq!(body["routes"]["items"]["items"], serde_json::json!([]));
    assert_eq!(body["routes"]["items"]["total"], 0);
}

#[tokio::test]
async fn falls_back_to_the_file_that_runs_something_at_module_level() {
    let project = library();
    let body = project.viewer().json("/api/entrypoints").await;
    let files = files_of(&body["files"]);
    assert!(files.contains(&"src/main.ts".to_string()), "{files:?}");
    assert!(!files.contains(&"__tests__/store.test.ts".to_string()));
    let main = body["files"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["file"] == "src/main.ts")
        .unwrap()
        .clone();
    assert!(main["calls"].as_i64().unwrap() > 0);
    assert!(main["reaches"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn lists_the_tests_by_what_they_exercise() {
    let project = library();
    let body = project.viewer().json("/api/entrypoints").await;
    let tests = files_of(&body["tests"]);
    assert!(
        tests.contains(&"__tests__/store.test.ts".to_string()),
        "{tests:?}"
    );
    assert!(!tests.contains(&"__tests__/fixtures/sample.ts".to_string()));
    let suite = body["tests"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["file"] == "__tests__/store.test.ts")
        .unwrap()
        .clone();
    assert!(suite["reaches"].as_i64().unwrap() > 0);
    assert!(suite["refs"].as_i64().unwrap() >= suite["reaches"].as_i64().unwrap());
}

#[tokio::test]
async fn counts_the_tests_exactly_and_the_derived_lists_as_a_floor() {
    let project = library();
    let body = project.viewer().json("/api/entrypoints").await;
    assert_eq!(
        body["tests"]["total"].as_u64().unwrap() as usize,
        body["tests"]["items"].as_array().unwrap().len()
    );
    for key in ["files", "hubs"] {
        assert!(
            body[key]["total"].as_u64().unwrap() as usize
                >= body[key]["items"].as_array().unwrap().len()
        );
    }
}
