//! `GET /api/map` against a real index — the API half of upstream
//! `__tests__/ui-map-api.test.ts` (`v1.6.1`). The pure helpers' cases live
//! beside them in `src/api/map.rs`.

mod support;

use axum::http::StatusCode;
use serde_json::Value;
use support::{Project, Viewer};

fn fixture() -> Project {
    Project::indexed(&[
        ("src/types.ts", "export interface Row {\n  id: string;\n}\n"),
        ("src/db/schema.ts", "export const TABLES = ['rows'];\n"),
        // db -> core, the LIGHT direction of the mutual pair below.
        (
            "src/db/store.ts",
            "import { Row } from '../types';
import { normalise } from '../core/util';

export class Store {
  rows: Row[] = [];
  put(row: Row): void {
    this.rows.push(normalise(row));
  }
}
",
        ),
        // util <-> store is a deliberate two-file import cycle.
        (
            "src/core/util.ts",
            "import { Row } from '../types';
import { Store } from '../db/store';

export function normalise(row: Row): Row {
  return { id: row.id.trim() };
}

export function count(store: Store): number {
  return store.rows.length;
}
",
        ),
        (
            "src/core/passes/trim.ts",
            "import { Row } from '../../types';

export function trim(row: Row): Row {
  return { id: row.id.slice(0, 8) };
}
",
        ),
        // core -> db, several times over: the HEAVY direction.
        (
            "src/core/engine.ts",
            "import { Store } from '../db/store';
import { TABLES } from '../db/schema';
import { trim } from './passes/trim';
import { Row } from '../types';

export class Engine {
  store = new Store();
  boot(): string[] {
    return TABLES;
  }
  add(row: Row): void {
    this.store.put(trim(row));
    this.store.put(row);
  }
}
",
        ),
        (
            "src/api/handler.ts",
            "import { Engine } from '../core/engine';
import { Row } from '../types';

export function handle(engine: Engine, row: Row): void {
  engine.add(row);
}
",
        ),
        (
            "src/api/routes.ts",
            "import { Engine } from '../core/engine';
import { handle } from './handler';

export function route(engine: Engine): void {
  handle(engine, { id: 'x' });
}
",
        ),
        (
            "src/index.ts",
            "import { Engine } from './core/engine';
import { route } from './api/routes';

export function start(): void {
  route(new Engine());
}
",
        ),
        (
            "__tests__/engine.test.ts",
            "import { Engine } from '../src/core/engine';

export function testBoot(): string[] {
  return new Engine().boot();
}
",
        ),
    ])
}

fn module<'a>(map: &'a Value, id: &str) -> &'a Value {
    map["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no module {id} in {}", map["modules"]))
}

fn link<'a>(map: &'a Value, source: &str, target: &str) -> Option<&'a Value> {
    map["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["source"] == source && l["target"] == target)
}

fn ids(map: &Value) -> Vec<String> {
    map["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect()
}

async fn map(viewer: &Viewer, query: &str) -> Value {
    viewer.json(&format!("/api/map{query}")).await
}

#[tokio::test]
async fn is_listed_by_the_api_index() {
    let project = fixture();
    let index = project.viewer().json("/api").await;
    assert!(
        index["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "/api/map")
    );
}

#[tokio::test]
async fn opens_on_the_source_directory_and_keeps_the_facade_its_own_box() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    assert_eq!(map["root"], "src");
    assert_eq!(map["depth"], 1);
    assert_eq!(
        ids(&map),
        [
            "src/(root files)",
            "src/api",
            "src/core",
            "src/db",
            "src/index.ts"
        ]
    );
    assert_eq!(module(&map, "src/core")["files"], 3);
    let facade = module(&map, "src/index.ts");
    assert_eq!(facade["facade"], true);
    assert_eq!(facade["files"], 1);
    assert!(facade["symbols"].as_i64().unwrap() > 0);
    assert!(
        map["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["test"] == false)
    );
}

#[tokio::test]
async fn offers_every_top_level_directory_as_a_root_plus_the_repository_itself() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    assert_eq!(
        map["roots"][0],
        serde_json::json!({ "root": "", "label": "whole repository", "files": map["index"]["files"] })
    );
    let roots: Vec<&str> = map["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["root"].as_str().unwrap())
        .collect();
    for root in ["", "src", "__tests__"] {
        assert!(roots.contains(&root), "{root}");
    }
}

#[tokio::test]
async fn counts_cross_module_edges_only_with_a_declared_subset_and_named_pairs() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    let link = link(&map, "src/api", "src/core").expect("src/api -> src/core");
    let count = link["count"].as_i64().unwrap();
    assert!(count > 0);
    let by_kind: i64 = link["byKind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["count"].as_i64().unwrap())
        .sum();
    assert_eq!(by_kind, count);
    let declared = link["declared"].as_i64().unwrap();
    assert!(declared > 0 && declared <= count);
    let pairs = link["topPairs"].as_array().unwrap();
    assert!(!pairs.is_empty() && pairs.len() <= 4);
    assert!(
        pairs
            .iter()
            .all(|p| p["declared"].as_i64() <= p["count"].as_i64())
    );
    assert!(
        map["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["source"] != l["target"])
    );
}

#[tokio::test]
async fn keeps_the_heavier_direction_of_a_mutual_pair_heavier() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    let core_to_db = link(&map, "src/core", "src/db").expect("core -> db");
    let db_to_core = link(&map, "src/db", "src/core").expect("db -> core");
    assert!(core_to_db["count"].as_i64() > db_to_core["count"].as_i64());
}

#[tokio::test]
async fn reports_the_file_level_cycle_the_fixture_contains() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    assert!(map["cycles"]["total"].as_i64().unwrap() >= 1);
    let knot = map["cycles"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            let files = c["files"].as_array().unwrap();
            files.contains(&Value::from("src/core/util.ts"))
                && files.contains(&Value::from("src/db/store.ts"))
        })
        .unwrap_or_else(|| panic!("{}", map["cycles"]));
    assert_eq!(
        knot["size"].as_u64().unwrap() as usize,
        knot["files"].as_array().unwrap().len()
    );
    let modules = knot["modules"].as_array().unwrap();
    assert!(modules.contains(&Value::from("src/core")) && modules.contains(&Value::from("src/db")));
    assert_eq!(
        map["cycles"]["shown"].as_u64().unwrap() as usize,
        map["cycles"]["items"].as_array().unwrap().len()
    );
}

#[tokio::test]
async fn lists_each_modules_files_capped_with_the_true_total_beside_them() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    for module in map["modules"].as_array().unwrap() {
        let list = &module["fileList"];
        assert_eq!(list["total"], module["files"]);
        assert_eq!(
            list["shown"].as_u64().unwrap() as usize,
            list["items"].as_array().unwrap().len()
        );
        assert_eq!(
            list["truncated"],
            list["shown"].as_u64() < list["total"].as_u64()
        );
        let items: Vec<&str> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap())
            .collect();
        let mut sorted = items.clone();
        sorted.sort();
        assert_eq!(items, sorted);
    }
    assert_eq!(
        module(&map, "src/core")["fileList"]["items"],
        serde_json::json!([
            "src/core/engine.ts",
            "src/core/passes/trim.ts",
            "src/core/util.ts"
        ])
    );
}

#[tokio::test]
async fn says_how_many_references_the_confidence_floor_excluded() {
    let project = fixture();
    let map = map(&project.viewer(), "").await;
    assert_eq!(map["excluded"]["confidenceBelow"], 0.6);
    assert!(map["excluded"]["uncertainEdges"].as_i64().unwrap() >= 0);
}

#[tokio::test]
async fn answers_the_whole_repository_where_the_tests_are_a_test_module() {
    let project = fixture();
    let map = map(&project.viewer(), "?root=&depth=1").await;
    assert_eq!(map["root"], "");
    assert_eq!(module(&map, "__tests__")["test"], true);
    assert_eq!(module(&map, "src")["test"], false);
    assert!(link(&map, "__tests__", "src").is_some());
}

#[tokio::test]
async fn splits_deeper_when_asked_and_src_slash_is_the_same_root_as_src() {
    let project = fixture();
    let viewer = project.viewer();
    let deep = map(&viewer, "?root=src&depth=2").await;
    let ids = ids(&deep);
    assert!(ids.contains(&"src/core/passes".to_string()));
    assert!(ids.contains(&"src/core/(root files)".to_string()));
    assert!(!ids.contains(&"src/core".to_string()));
    assert!(ids.contains(&"src/api".to_string()));
    assert!(!ids.contains(&"src/api/(root files)".to_string()));
    let slashed = map(&viewer, "?root=src%2F&depth=2").await;
    assert_eq!(slashed["modules"], deep["modules"]);
}

#[tokio::test]
async fn counts_the_files_outside_each_module_that_reference_into_it() {
    let project = fixture();
    let map = map(&project.viewer(), "?root=src&depth=1").await;
    let types = module(&map, "src/(root files)");
    assert!(types["dependents"]["files"].as_i64().unwrap() > 0);
    assert!(types["dependents"]["modules"].as_i64().unwrap() > 1);
    let modules = map["modules"].as_array().unwrap();
    let total: i64 = modules.iter().map(|m| m["files"].as_i64().unwrap()).sum();
    for module in modules {
        assert!(
            module["dependents"]["files"].as_i64().unwrap()
                <= total - module["files"].as_i64().unwrap()
        );
        assert!(module["dependents"]["modules"].as_i64().unwrap() < modules.len() as i64);
        let arrives = map["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["target"] == module["id"]);
        if !arrives {
            assert_eq!(module["dependents"]["files"], 0, "{}", module["id"]);
        }
    }
}

#[tokio::test]
async fn rejects_an_out_of_range_depth_as_json_not_as_a_crash() {
    let project = fixture();
    let reply = project.viewer().get("/api/map?depth=9").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(reply.json()["code"], "bad-request");
    assert!(reply.json()["error"].as_str().unwrap().contains("depth"));
}

#[tokio::test]
async fn serves_the_second_identical_request_from_the_cache_byte_for_byte() {
    let project = fixture();
    let viewer = project.viewer();
    let mut first = map(&viewer, "?root=src&depth=1").await;
    let mut second = map(&viewer, "?root=src&depth=1").await;
    assert_eq!(first["timing"]["cached"], false);
    assert_eq!(second["timing"]["cached"], true);
    first.as_object_mut().unwrap().remove("timing");
    second.as_object_mut().unwrap().remove("timing");
    assert_eq!(
        serde_json::to_string(&second).unwrap(),
        serde_json::to_string(&first).unwrap()
    );
}

#[tokio::test]
async fn does_not_let_one_roots_answer_be_served_for_another() {
    let project = fixture();
    let viewer = project.viewer();
    let src = map(&viewer, "?root=src&depth=1").await;
    let all = map(&viewer, "?root=&depth=1").await;
    assert_eq!(all["root"], "");
    assert_ne!(ids(&all), ids(&src));
}
