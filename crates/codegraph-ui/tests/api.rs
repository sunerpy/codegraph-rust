//! The viewer's read-only JSON API against a real indexed fixture — a port of
//! upstream `__tests__/ui-server-api.test.ts` (`v1.6.1`), case by case, each
//! test named after the upstream `it(...)` it ports.
//!
//! The fixture is upstream's: a call chain three deep, a test file that reaches
//! it, a type used only as a type, an import that cannot resolve, a file that
//! runs something at its top level, a CRLF file, and one function with 500
//! callers.

mod support;

use std::time::Instant;

use axum::http::StatusCode;
use serde_json::Value;
use support::{Project, encode};

const TYPES_TS: &str = "export interface Config {
  ttlMs: number;
  label: string;
}

export type CacheKey = string;
";

const CACHE_TS: &str = "import { Config, CacheKey } from './types';

export class Cache {
  private store = new Map<string, string>();
  private config: Config;

  constructor(config: Config) {
    this.config = config;
  }

  read(key: CacheKey): string | undefined {
    return this.store.get(key);
  }

  write(key: CacheKey, value: string): void {
    this.store.set(key, value);
  }
}
";

const SERVICE_TS: &str = "import { Cache } from './cache';
import { Config } from './types';
// Not in the index: a package that was never installed here.
import { serialize } from 'some-external-package';

export class Service {
  private cache: Cache;

  constructor(config: Config) {
    this.cache = new Cache(config);
  }

  load(key: string): string {
    const hit = this.cache.read(key);
    if (hit !== undefined) return hit;
    const fresh = serialize(key);
    this.cache.write(key, fresh);
    return fresh;
  }
}
";

const HANDLER_TS: &str = "import { Service } from './service';

export function handleRequest(service: Service, key: string): string {
  return service.load(key);
}
";

const MAIN_TS: &str = "import { Service } from './service';
import { handleRequest } from './handler';

const service = new Service({ ttlMs: 5, label: 'main' });
const first = handleRequest(service, 'boot');
const second = service.load('warm');

export const started = [first, second];
";

const SERVICE_TEST_TS: &str = "import { Service } from '../src/service';

export function testLoadsThroughCache(): void {
  const service = new Service({ ttlMs: 1, label: 'x' });
  service.load('k');
}

// Module level, on purpose: a test file that RUNS something must still be
// excluded from the entry points.
testLoadsThroughCache();
";

fn hot_ts() -> String {
    let callers: Vec<String> = (0..500)
        .map(|i| format!("export function caller{i}(): number {{\n  return hot({i});\n}}"))
        .collect();
    format!(
        "export function hot(n: number): number {{\n  return n * 2;\n}}\n\n{}\n",
        callers.join("\n\n")
    )
}

/// CRLF on purpose: the graph numbers rows by `\n`, so a CRLF file must come
/// back on the same numbering, without a stray `\r` on every line.
const CRLF_TS: &str =
    "export function windowsStyle(n: number): number {\r\n  return n + 1;\r\n}\r\n";

fn fixture() -> Project {
    let hot = hot_ts();
    Project::indexed(&[
        ("src/types.ts", TYPES_TS),
        ("src/cache.ts", CACHE_TS),
        ("src/service.ts", SERVICE_TS),
        ("src/handler.ts", HANDLER_TS),
        ("src/main.ts", MAIN_TS),
        ("src/hot.ts", &hot),
        ("src/crlf.ts", CRLF_TS),
        ("__tests__/service.test.ts", SERVICE_TEST_TS),
    ])
}

fn names(list: &Value, at: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| {
            item.pointer(at)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

fn is_sorted(values: &[i64]) -> bool {
    values.windows(2).all(|w| w[0] <= w[1])
}

fn ints(list: &Value, at: &str) -> Vec<i64> {
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.pointer(at).and_then(Value::as_i64))
        .collect()
}

/* ------------------------------------------------------------------ /api -- */

#[tokio::test]
async fn lists_the_endpoints_it_answers() {
    let project = fixture();
    let body = project.viewer().json("/api").await;
    assert_eq!(body["readOnly"], false);
    assert_eq!(
        body["writes"],
        serde_json::json!(["POST /api/trails", "DELETE /api/trails/<id>"])
    );
    let paths = names(&body["endpoints"], "/path");
    for path in [
        "/api/stats",
        "/api/search",
        "/api/node/<id>",
        "/api/source",
        "/api/file/<path>",
        "/api/routes",
    ] {
        assert!(paths.iter().any(|p| p == path), "{path}");
    }
    // Steps is not built by this server, so it is not listed.
    assert!(!paths.iter().any(|p| p == "/api/steps"));
}

#[tokio::test]
async fn _404s_an_unknown_endpoint_as_json_never_as_the_app_shell() {
    let project = fixture();
    let reply = project.viewer().get("/api/nope").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.json()["code"], "not-found");
    let steps = project.viewer().get("/api/steps").await;
    assert_eq!(steps.status, StatusCode::NOT_FOUND);
    assert!(steps.json()["hint"].as_str().unwrap().contains("Steps"));
}

#[tokio::test]
async fn answers_head_with_the_headers_and_no_body() {
    let project = fixture();
    let reply = project
        .viewer()
        .request("HEAD", "/api/stats", &[], None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    let length: usize = reply.header("content-length").unwrap().parse().unwrap();
    assert!(length > 0);
    assert!(reply.body.is_empty());
}

/* ------------------------------------------------------------ /api/stats -- */

#[tokio::test]
async fn reports_the_project_the_index_state_and_the_graph_counts() {
    let project = fixture();
    let body = project.viewer().json("/api/stats").await;
    assert_eq!(
        body["project"]["root"],
        project.root().display().to_string()
    );
    assert_eq!(body["project"]["name"], "project");
    assert_eq!(body["index"]["state"], "complete");
    assert_eq!(body["index"]["stale"], false);
    assert!(body["index"]["lastIndexedAt"].is_number());
    assert_eq!(body["index"]["backend"], "rusqlite");
    assert!(body["index"]["extractionVersion"].is_number());
    assert!(body["graph"]["nodes"].as_i64().unwrap() > 0);
    assert!(body["graph"]["edges"].as_i64().unwrap() > 0);
    assert!(body["graph"]["files"].as_i64().unwrap() >= 6);
    assert!(body["graph"]["nodesByKind"]["class"].as_i64().unwrap() >= 2);
    assert!(
        body["graph"]["filesByLanguage"]["typescript"]
            .as_i64()
            .unwrap()
            >= 6
    );
    assert_eq!(
        body["thresholds"],
        serde_json::json!({ "hub": 40, "uncertainBelow": 0.6 })
    );
}

#[tokio::test]
async fn reports_a_blast_radius_scale_the_widest_symbol_in_the_index_reaches() {
    let project = fixture();
    let scale = project.viewer().json("/api/stats").await["blastScale"].clone();
    assert_eq!(scale["maxDirect"], 500);
    assert!(scale["maxWithinHops"].as_i64().unwrap() >= 500);
    assert_eq!(scale["hops"], 3);
    let sampled = scale["sampled"].as_i64().unwrap();
    assert!(sampled > 0 && sampled <= 24);
    assert_eq!(scale["estimated"], true);
}

#[tokio::test]
async fn serves_the_scale_from_cache_the_second_call_does_not_re_traverse() {
    let project = fixture();
    let viewer = project.viewer();
    let first = viewer.json("/api/stats").await;
    let started = Instant::now();
    let second = viewer.json("/api/stats").await;
    assert_eq!(second["blastScale"], first["blastScale"]);
    // A smoke test for the memo existing at all, not a benchmark: 24 depth-3
    // traversals over the 500-caller graph are not free; a cached answer is.
    assert!(
        started.elapsed().as_millis() < 1500,
        "{:?}",
        started.elapsed()
    );
}

/* ----------------------------------------------------------- /api/search -- */

#[tokio::test]
async fn ranks_exact_over_prefix_over_substring_and_groups_by_kind() {
    let project = fixture();
    let body = project.viewer().json("/api/search?q=Cache").await;
    let first = &body["results"]["items"][0];
    assert_eq!(first["name"], "Cache");
    assert_eq!(first["kind"], "class");
    assert_eq!(first["matchKind"], "exact");
    let order = [
        "exact",
        "prefix",
        "substring",
        "qualified",
        "file",
        "related",
    ];
    let ranks: Vec<i64> = names(&body["results"]["items"], "/matchKind")
        .iter()
        .map(|m| order.iter().position(|o| o == m).unwrap() as i64)
        .collect();
    assert!(is_sorted(&ranks), "{ranks:?}");
    let mut flattened: Vec<String> = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| names(&g["items"], "/id"))
        .collect();
    let mut flat = names(&body["results"]["items"], "/id");
    flattened.sort();
    flat.sort();
    assert_eq!(flattened, flat);
    for group in body["groups"].as_array().unwrap() {
        assert_eq!(
            group["count"].as_u64().unwrap() as usize,
            group["items"].as_array().unwrap().len()
        );
    }
}

#[tokio::test]
async fn returns_a_signature_and_a_file_line_for_every_result() {
    let project = fixture();
    let body = project.viewer().json("/api/search?q=handleRequest").await;
    let hit = body["results"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "handleRequest")
        .unwrap()
        .clone();
    assert_eq!(hit["file"], "src/handler.ts");
    assert!(hit["line"].as_i64().unwrap() > 0);
    assert!(hit["endLine"].as_i64().unwrap() >= hit["line"].as_i64().unwrap());
    assert!(hit["signature"].as_str().unwrap().contains("service"));
    assert!(!hit["qualifiedName"].as_str().unwrap().is_empty());
    assert_eq!(hit["language"], "typescript");
}

#[tokio::test]
async fn finds_a_mid_name_match_fts_tokens_cannot() {
    let project = fixture();
    let body = project.viewer().json("/api/search?q=quest").await;
    let hit = body["results"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "handleRequest")
        .expect("handleRequest by substring")
        .clone();
    assert_eq!(hit["matchKind"], "substring");
}

#[tokio::test]
async fn honours_the_kind_filter_grammar() {
    let project = fixture();
    let body = project
        .viewer()
        .json(&format!("/api/search?q={}", encode("kind:class Cache")))
        .await;
    assert_eq!(body["filters"]["kinds"], serde_json::json!(["class"]));
    assert!(
        body["results"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] == "class")
    );
}

#[tokio::test]
async fn marks_test_files_so_the_palette_can_rank_them_down() {
    let project = fixture();
    let body = project
        .viewer()
        .json("/api/search?q=testLoadsThroughCache")
        .await;
    let hit = body["results"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "testLoadsThroughCache")
        .unwrap()
        .clone();
    assert_eq!(hit["test"], true);
}

#[tokio::test]
async fn answers_an_empty_search_box_with_nothing_and_a_missing_q_with_400() {
    let project = fixture();
    let viewer = project.viewer();
    let empty = viewer.get("/api/search?q=").await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.json()["results"]["total"], 0);
    assert_eq!(empty.json()["groups"], serde_json::json!([]));
    let missing = viewer.get("/api/search").await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.json()["code"], "bad-request");
}

#[tokio::test]
async fn returns_an_empty_result_set_for_a_name_nothing_has() {
    let project = fixture();
    let body = project
        .viewer()
        .json("/api/search?q=zzznotasymbolanywhere")
        .await;
    assert_eq!(body["results"]["total"], 0);
}

/* -------------------------------------------------------- /api/node/<id> -- */

#[tokio::test]
async fn returns_the_symbol_its_ancestors_and_its_members_in_source_order() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Cache", Some("class")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    assert_eq!(body["node"]["name"], "Cache");
    assert_eq!(body["node"]["kind"], "class");
    assert_eq!(body["node"]["file"], "src/cache.ts");
    assert_eq!(
        body["node"]["lines"].as_i64().unwrap(),
        body["node"]["endLine"].as_i64().unwrap() - body["node"]["line"].as_i64().unwrap() + 1
    );
    assert_eq!(body["node"]["exported"], true);
    assert_eq!(body["ancestors"][0]["kind"], "file");
    assert_eq!(body["ancestors"][0]["file"], "src/cache.ts");
    let members = names(&body["members"]["items"], "/name");
    for name in ["read", "write", "store", "config"] {
        assert!(members.iter().any(|m| m == name), "{name} in {members:?}");
    }
    assert!(is_sorted(&ints(&body["members"]["items"], "/line")));
    for member in body["members"]["items"].as_array().unwrap() {
        assert_eq!(member["parentId"], body["node"]["id"]);
        assert_eq!(member["depth"], 1);
    }
    assert_eq!(body["members"]["total"], body["members"]["shown"]);
}

#[tokio::test]
async fn gives_every_member_its_own_fan_in_and_fan_out_the_outline_is_the_body() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Cache", Some("class")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let members = body["members"]["items"].as_array().unwrap();
    for member in members {
        assert!(member["fanIn"].as_i64().unwrap() >= 0);
        assert!(member["fanOut"].as_i64().unwrap() >= 0);
    }
    let fan_in = |name: &str| {
        members.iter().find(|m| m["name"] == name).unwrap()["fanIn"]
            .as_i64()
            .unwrap()
    };
    // `Service.load` calls both, and `Cache` contains them.
    assert!(fan_in("read") >= 2);
    assert!(fan_in("write") >= 2);
    assert_eq!(body["counts"]["callees"], 0);
    assert!(members.iter().any(|m| m["fanOut"].as_i64().unwrap() > 0));
}

#[tokio::test]
async fn nests_a_file_outline_one_level_deeper_so_a_class_shows_its_methods() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("cache.ts", Some("file")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let at = |depth: i64| -> Vec<String> {
        body["members"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["depth"] == depth)
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(at(1).contains(&"Cache".to_string()));
    for name in ["read", "write"] {
        assert!(at(2).contains(&name.to_string()), "{name}");
    }
}

#[tokio::test]
async fn groups_incoming_edges_by_the_calling_symbol_with_their_call_sites() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("read", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let from_load = body["incoming"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["node"]["name"] == "load")
        .expect("Service.load should call Cache.read")
        .clone();
    assert_eq!(from_load["node"]["file"], "src/service.ts");
    assert!(
        from_load["edgeKinds"]
            .as_array()
            .unwrap()
            .contains(&Value::from("calls"))
    );
    assert!(from_load["edgeCount"].as_i64().unwrap() >= 1);
    let lines: Vec<i64> = from_load["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    assert!(!lines.is_empty());
    assert!(is_sorted(&lines));
    assert!(from_load["fanIn"].is_number());
    assert_eq!(from_load["hub"], false);
}

#[tokio::test]
async fn carries_every_edge_attribute_the_viewer_draws_with() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("read", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let relation = body["incoming"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["node"]["name"] == "load")
        .unwrap()
        .clone();
    let edge = &relation["edges"][0];
    assert_eq!(edge["kind"], "calls");
    assert!(edge["line"].is_number());
    assert!(edge["col"].is_number());
    assert!(edge["confidence"].is_number());
    assert!(edge["resolvedBy"].is_string());
    let max = relation["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["confidence"].as_f64().unwrap_or(-1.0))
        .fold(f64::MIN, f64::max);
    assert_eq!(relation["confidence"].as_f64().unwrap(), max);
    assert_eq!(
        relation["uncertain"],
        relation["confidence"].as_f64().unwrap() < 0.6
    );
    assert_eq!(relation["synthesized"], false);
}

#[tokio::test]
async fn groups_outgoing_edges_by_the_called_symbol_ordered_by_call_site() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("load", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let called = names(&body["outgoing"]["items"], "/node/name");
    for name in ["read", "write"] {
        assert!(called.iter().any(|c| c == name), "{name} in {called:?}");
    }
    assert!(is_sorted(&ints(&body["outgoing"]["items"], "/lines/0")));
}

#[tokio::test]
async fn splits_type_references_out_of_the_callee_rail() {
    let project = fixture();
    let viewer = project.viewer();
    let service = viewer.id_of("Service", Some("class")).await;
    let service = viewer
        .json(&format!("/api/node/{}", encode(&service)))
        .await;
    let ctor = service["members"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "constructor")
        .expect("a constructor member")
        .clone();
    let body = viewer
        .json(&format!(
            "/api/node/{}",
            encode(ctor["id"].as_str().unwrap())
        ))
        .await;
    let types = names(&body["typesUsed"], "/node/name");
    assert!(types.iter().any(|t| t == "Config"), "{types:?}");
    assert!(body["typesUsed"].as_array().unwrap().iter().all(|t| {
        t["edgeKinds"]
            .as_array()
            .unwrap()
            .contains(&Value::from("references"))
    }));
    let callees = names(&body["outgoing"]["items"], "/node/name");
    assert!(!callees.iter().any(|c| c == "Config"));
    let instantiated = body["outgoing"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["node"]["name"] == "Cache")
        .expect("new Cache(...) stays on the callee rail")
        .clone();
    assert!(
        instantiated["edgeKinds"]
            .as_array()
            .unwrap()
            .contains(&Value::from("instantiates"))
    );
}

#[tokio::test]
async fn summarizes_which_tests_reach_the_symbol() {
    let project = fixture();
    let viewer = project.viewer();
    let load = viewer.id_of("load", Some("method")).await;
    let reached = viewer.json(&format!("/api/node/{}", encode(&load))).await;
    assert_eq!(reached["tests"]["reached"], true);
    assert_eq!(reached["tests"]["hops"], 1);
    assert!(
        reached["tests"]["files"]
            .as_array()
            .unwrap()
            .contains(&Value::from("__tests__/service.test.ts"))
    );
    assert!(reached["tests"]["fileCount"].as_i64().unwrap() >= 1);
    assert!(reached["tests"]["files"].as_array().unwrap().len() <= 6);
    assert_eq!(reached["tests"]["exhaustive"], true);

    let hot = viewer.id_of("hot", Some("function")).await;
    let unreached = viewer.json(&format!("/api/node/{}", encode(&hot))).await;
    assert_eq!(unreached["tests"]["reached"], false);
    assert!(unreached["tests"]["hops"].is_null());
    assert_eq!(unreached["tests"]["files"], serde_json::json!([]));
}

#[tokio::test]
async fn counts_the_calls_that_leave_the_index_instead_of_hiding_them() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("load", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    assert!(body["outsideIndex"]["total"].as_i64().unwrap() > 0);
    let samples = names(&body["outsideIndex"]["samples"], "/name");
    assert!(samples.iter().any(|s| s == "serialize"), "{samples:?}");
    for sample in body["outsideIndex"]["samples"].as_array().unwrap() {
        assert!(sample["line"].is_number());
        assert!(sample["kind"].is_string());
    }
}

#[tokio::test]
async fn summarizes_the_blast_radius_at_three_hops() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("read", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let blast = &body["blast"];
    assert_eq!(blast["hops"], 3);
    assert_eq!(blast["direct"], body["counts"]["callers"]);
    assert!(blast["withinHops"].as_i64().unwrap() > blast["direct"].as_i64().unwrap());
    assert!(blast["files"].as_i64().unwrap() >= 2);
    assert!(blast["testFiles"].as_i64().unwrap() >= 1);
    assert_eq!(blast["routes"], 0);
    assert!(blast["topFiles"][0]["symbols"].as_i64().unwrap() >= 1);
}

#[tokio::test]
async fn keeps_every_count_equal_to_the_list_it_labels() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("read", Some("method")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    assert_eq!(body["counts"]["callers"], body["incoming"]["total"]);
    assert_eq!(body["counts"]["callees"], body["outgoing"]["total"]);
    assert_eq!(
        body["counts"]["typesUsed"].as_u64().unwrap() as usize,
        body["typesUsed"].as_array().unwrap().len()
    );
    assert_eq!(body["counts"]["members"], body["members"]["total"]);
    assert_eq!(body["blast"]["direct"], body["counts"]["callers"]);
}

#[tokio::test]
async fn reports_fan_in_fan_out_and_the_hub_flag() {
    let project = fixture();
    let viewer = project.viewer();
    let write = viewer.id_of("write", Some("method")).await;
    let quiet = viewer.json(&format!("/api/node/{}", encode(&write))).await;
    assert_eq!(quiet["counts"]["hub"], false);
    assert!(quiet["counts"]["callers"].as_i64().unwrap() < 40);
    assert!(
        quiet["counts"]["fanIn"].as_i64().unwrap() >= quiet["counts"]["callers"].as_i64().unwrap()
    );
    let hot = viewer.id_of("hot", Some("function")).await;
    let hot = viewer.json(&format!("/api/node/{}", encode(&hot))).await;
    assert_eq!(hot["counts"]["hub"], true);
    assert!(hot["counts"]["callers"].as_i64().unwrap() >= 500);
}

#[tokio::test]
async fn flags_nothing_as_drifted_while_the_fixture_is_untouched() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("read", Some("method")).await;
    assert_eq!(
        viewer.json(&format!("/api/node/{}", encode(&id))).await["drift"],
        false
    );
}

#[tokio::test]
async fn _404s_an_id_that_names_nothing_and_400s_an_empty_one() {
    let project = fixture();
    let viewer = project.viewer();
    let missing = viewer.get("/api/node/method:notarealid").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["code"], "not-found");
    assert!(missing.json()["hint"].is_string());
    // `/api/node/` normalises to `/api/node`, which names no argument.
    let empty = viewer.get("/api/node/").await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn is_null_for_a_function_so_the_block_costs_a_plain_symbol_nothing() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("hot", Some("function")).await;
    assert!(viewer.json(&format!("/api/node/{}", encode(&id))).await["hierarchy"].is_null());
}

#[tokio::test]
async fn is_null_for_a_class_with_nothing_above_or_below_it() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Cache", Some("class")).await;
    assert!(viewer.json(&format!("/api/node/{}", encode(&id))).await["hierarchy"].is_null());
}

#[tokio::test]
async fn caps_the_caller_list_keeps_the_true_total_and_stays_fast() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("hot", Some("function")).await;
    let path = format!("/api/node/{}", encode(&id));
    let _ = viewer.get(&path).await;
    let started = Instant::now();
    let reply = viewer.get(&path).await;
    let elapsed = started.elapsed();
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert!(body["incoming"]["total"].as_i64().unwrap() >= 500);
    assert_eq!(body["incoming"]["shown"], 300);
    assert_eq!(body["incoming"]["truncated"], true);
    assert_eq!(body["incoming"]["items"].as_array().unwrap().len(), 300);
    let distinct: std::collections::HashSet<String> = names(&body["incoming"]["items"], "/node/id")
        .into_iter()
        .collect();
    assert_eq!(distinct.len(), 300);
    assert!(body["counts"]["callers"].as_i64().unwrap() >= 500);
    assert_eq!(body["blast"]["direct"], body["counts"]["callers"]);
    // 500 callers resolved one query at a time would be nowhere near this, even
    // in a debug build.
    assert!(elapsed.as_millis() < 3000, "{elapsed:?}");
}

/* ----------------------------------------------------------- /api/source -- */

#[tokio::test]
async fn returns_the_requested_slice_with_the_index_line_numbering() {
    let project = fixture();
    let body = project
        .viewer()
        .json("/api/source?file=src/cache.ts&from=1&to=3")
        .await;
    assert_eq!(body["drift"], false);
    assert_eq!(body["file"], "src/cache.ts");
    assert_eq!(body["language"], "typescript");
    assert_eq!(body["from"], 1);
    assert_eq!(body["to"], 3);
    assert_eq!(body["lines"].as_array().unwrap().len(), 3);
    assert!(
        body["lines"][0]
            .as_str()
            .unwrap()
            .contains("import { Config, CacheKey } from './types'")
    );
    assert!(body["totalLines"].as_i64().unwrap() > 3);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn serves_the_whole_file_when_no_range_is_given() {
    let project = fixture();
    let body = project
        .viewer()
        .json("/api/source?file=src/handler.ts")
        .await;
    assert_eq!(body["from"], 1);
    assert_eq!(body["to"], body["totalLines"]);
    assert_eq!(
        body["lines"].as_array().unwrap().len() as i64,
        body["totalLines"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn slices_exactly_the_lines_a_symbol_claims() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("handleRequest", Some("function")).await;
    let node = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let body = viewer
        .json(&format!(
            "/api/source?file={}&from={}&to={}",
            node["node"]["file"].as_str().unwrap(),
            node["node"]["line"],
            node["node"]["endLine"]
        ))
        .await;
    assert!(body["lines"][0].as_str().unwrap().contains("handleRequest"));
    assert_eq!(
        body["lines"].as_array().unwrap().len() as i64,
        node["node"]["lines"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn carries_the_classified_source_beside_the_lines_one_entry_per_line() {
    let project = fixture();
    let body = project
        .viewer()
        .json("/api/source?file=src/cache.ts&from=1&to=3")
        .await;
    let highlight = &body["highlight"];
    assert_eq!(
        highlight["classes"],
        serde_json::json!([
            "other", "ident", "comment", "string", "keyword", "number", "type", "def"
        ])
    );
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(highlight["lines"].as_array().unwrap().len(), lines.len());
    for (i, line) in lines.iter().enumerate() {
        let rebuilt: String = highlight["lines"][i]
            .as_array()
            .unwrap()
            .iter()
            .map(|token| token[1].as_str().unwrap())
            .collect();
        assert_eq!(rebuilt, line.as_str().unwrap());
    }
}

#[tokio::test]
async fn refuses_to_slice_a_file_that_changed_on_disk_after_the_last_sync() {
    let project = fixture();
    let viewer = project.viewer();
    let original = std::fs::read_to_string(project.root().join("src/handler.ts")).unwrap();
    project.write(
        "src/handler.ts",
        &format!("// a new first line\n{original}"),
    );
    let body = viewer
        .json("/api/source?file=src/handler.ts&from=1&to=3")
        .await;
    assert_eq!(body["drift"], true);
    assert!(body.get("lines").is_none());
    assert!(body.get("highlight").is_none());
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("changed on disk after the last index sync")
    );
    let id = viewer.id_of("handleRequest", Some("function")).await;
    assert_eq!(
        viewer.json(&format!("/api/node/{}", encode(&id))).await["drift"],
        true
    );
    assert_eq!(viewer.json("/api/file/src/handler.ts").await["drift"], true);
}

#[tokio::test]
async fn does_not_call_an_identical_rewrite_drift() {
    let project = fixture();
    let original = std::fs::read(project.root().join("src/handler.ts")).unwrap();
    // Same bytes, new mtime — what a checkout or a formatter no-op looks like.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(project.root().join("src/handler.ts"), original).unwrap();
    let body = project
        .viewer()
        .json("/api/source?file=src/handler.ts&from=1&to=2")
        .await;
    assert_eq!(body["drift"], false);
    assert_eq!(body["lines"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn keeps_a_crlf_file_on_the_index_line_numbering_without_the_stray_carriage_returns() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("windowsStyle", Some("function")).await;
    let node = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    assert_eq!(node["node"]["file"], "src/crlf.ts");
    let body = viewer.json("/api/source?file=src/crlf.ts").await;
    assert_eq!(body["drift"], false);
    assert_eq!(body["totalLines"], 3);
    assert_eq!(
        body["lines"],
        serde_json::json!([
            "export function windowsStyle(n: number): number {",
            "  return n + 1;",
            "}"
        ])
    );
    let slice = viewer
        .json(&format!(
            "/api/source?file=src/crlf.ts&from={}&to={}",
            node["node"]["line"], node["node"]["endLine"]
        ))
        .await;
    assert!(slice["lines"][0].as_str().unwrap().contains("windowsStyle"));
}

#[tokio::test]
async fn refuses_a_path_that_escapes_the_project() {
    let project = fixture();
    let viewer = project.viewer();
    let traversal = viewer
        .get(&format!(
            "/api/source?file={}&from=1",
            encode("../../../etc/passwd")
        ))
        .await;
    assert_eq!(traversal.status, StatusCode::FORBIDDEN);
    assert_eq!(traversal.json()["code"], "refused");
    let absolute = viewer
        .get(&format!(
            "/api/source?file={}&from=1",
            encode("/etc/passwd")
        ))
        .await;
    assert_eq!(absolute.status, StatusCode::FORBIDDEN);
    assert_eq!(absolute.json()["code"], "refused");
    assert!(
        absolute.json()["error"]
            .as_str()
            .unwrap()
            .contains("absolute")
    );
}

#[tokio::test]
async fn refuses_a_nul_byte_in_the_path() {
    let project = fixture();
    let reply = project
        .viewer()
        .get(&format!(
            "/api/source?file={}&from=1",
            encode("src/cache.ts\u{0}.png")
        ))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["code"], "refused");
}

#[tokio::test]
async fn _404s_a_file_that_exists_but_is_not_indexed() {
    let project = fixture();
    project.write("notes.md", "# not indexed\n");
    let reply = project.viewer().get("/api/source?file=notes.md").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.json()["code"], "not-found");
    assert!(reply.json()["hint"].as_str().unwrap().contains("index"));
}

#[tokio::test]
async fn rejects_a_range_that_names_nothing() {
    let project = fixture();
    let viewer = project.viewer();
    let past = viewer
        .get("/api/source?file=src/handler.ts&from=99999")
        .await;
    assert_eq!(past.status, StatusCode::BAD_REQUEST);
    assert!(
        past.json()["error"]
            .as_str()
            .unwrap()
            .contains("past the end")
    );
    let backwards = viewer
        .get("/api/source?file=src/handler.ts&from=10&to=4")
        .await;
    assert_eq!(backwards.status, StatusCode::BAD_REQUEST);
    let non_numeric = viewer.get("/api/source?file=src/handler.ts&from=abc").await;
    assert_eq!(non_numeric.status, StatusCode::BAD_REQUEST);
}

/* ------------------------------------------------------- /api/file/<path> -- */

#[tokio::test]
async fn returns_the_file_record_and_its_outline_in_source_order() {
    let project = fixture();
    let body = project.viewer().json("/api/file/src/cache.ts").await;
    assert_eq!(body["file"]["path"], "src/cache.ts");
    assert_eq!(body["file"]["language"], "typescript");
    assert!(body["file"]["size"].as_i64().unwrap() > 0);
    let hash = body["file"]["contentHash"].as_str().unwrap();
    assert!(hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(body["file"]["generated"], false);
    assert_eq!(body["file"]["test"], false);
    assert!(body["file"]["id"].as_str().unwrap().starts_with("file:"));
    assert_eq!(body["drift"], false);
    let items = body["outline"]["items"].as_array().unwrap();
    assert!(is_sorted(&ints(&body["outline"]["items"], "/line")));
    let cache = items.iter().find(|o| o["name"] == "Cache").unwrap();
    assert_eq!(cache["depth"], 0);
    assert!(cache["parentId"].is_null());
    let read = items.iter().find(|o| o["name"] == "read").unwrap();
    assert_eq!(read["depth"], 1);
    assert_eq!(read["parentId"], cache["id"]);
    assert!(read["fanIn"].as_i64().unwrap() >= 1);
    assert!(read["fanOut"].is_number());
    assert!(
        !items
            .iter()
            .any(|o| o["kind"] == "file" || o["kind"] == "import")
    );
}

#[tokio::test]
async fn maps_imports_and_imported_by_to_files() {
    let project = fixture();
    let body = project.viewer().json("/api/file/src/cache.ts").await;
    let imported_by = names(&body["importedBy"]["items"], "/file");
    let imports = names(&body["imports"]["items"], "/file");
    assert!(
        imported_by.iter().any(|f| f == "src/service.ts"),
        "{imported_by:?}"
    );
    assert!(imports.iter().any(|f| f == "src/types.ts"), "{imports:?}");
    assert!(!imports.iter().any(|f| f == "src/cache.ts"));
    assert!(!imported_by.iter().any(|f| f == "src/cache.ts"));
    let types = body["imports"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["file"] == "src/types.ts")
        .unwrap()
        .clone();
    assert!(types["symbolCount"].as_i64().unwrap() >= 1);
    assert!(types["symbols"][0]["name"].is_string());
    assert!(types["symbols"][0]["id"].is_string());
    assert_eq!(types["test"], false);
}

#[tokio::test]
async fn names_the_imports_that_never_resolved_rather_than_dropping_them() {
    let project = fixture();
    let body = project.viewer().json("/api/file/src/service.ts").await;
    let unresolved = names(&body["unresolvedImports"], "/name");
    // KEEP-RUST: the Rust extractor records an unresolved import by the binding
    // it names (`serialize`), where upstream's records the module specifier
    // (`some-external-package`); the viewer lists whatever the index holds.
    assert!(
        unresolved.iter().any(|n| n == "serialize"),
        "{unresolved:?}"
    );
    assert_eq!(body["unresolvedImports"][0]["line"], 4);
}

#[tokio::test]
async fn reports_the_wider_cross_file_relationship_too() {
    let project = fixture();
    let body = project.viewer().json("/api/file/src/cache.ts").await;
    assert!(
        body["dependents"]
            .as_array()
            .unwrap()
            .contains(&Value::from("src/service.ts"))
    );
    assert!(
        body["dependencies"]
            .as_array()
            .unwrap()
            .contains(&Value::from("src/types.ts"))
    );
}

#[tokio::test]
async fn says_whether_the_file_runs_anything_at_its_top_level() {
    let project = fixture();
    let viewer = project.viewer();
    assert!(
        viewer.json("/api/file/src/main.ts").await["topLevel"]["calls"]
            .as_i64()
            .unwrap()
            >= 2
    );
    assert_eq!(
        viewer.json("/api/file/src/cache.ts").await["topLevel"]["calls"],
        0
    );
}

#[tokio::test]
async fn _404s_a_file_that_is_not_in_the_index_and_refuses_one_outside_the_project() {
    let project = fixture();
    let viewer = project.viewer();
    let missing = viewer.get("/api/file/src/nope.ts").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["code"], "not-found");
    let outside = viewer
        .get(&format!("/api/file/{}", encode("/etc/passwd")))
        .await;
    assert_eq!(outside.status, StatusCode::FORBIDDEN);
    assert_eq!(outside.json()["code"], "refused");
}

/* ----------------------------------------------------------- /api/routes -- */

#[tokio::test]
async fn says_plainly_that_this_project_is_not_a_routed_app() {
    let project = fixture();
    let body = project.viewer().json("/api/routes").await;
    assert_eq!(body["routed"], false);
    assert_eq!(body["entries"], serde_json::json!([]));
    assert_eq!(body["routeCount"], 0);
    assert_eq!(body["shown"], 0);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn refuses_a_limit_the_manifest_cannot_answer_truthfully() {
    let project = fixture();
    let viewer = project.viewer();
    for limit in ["0", "2", "-1", "abc"] {
        let reply = viewer.get(&format!("/api/routes?limit={limit}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "limit={limit}");
        assert_eq!(reply.json()["code"], "bad-request");
    }
}

/* ------------------------------------------------------ /api/entrypoints -- */

#[tokio::test]
async fn finds_the_file_that_runs_something_and_reports_what_it_reaches() {
    let project = fixture();
    let body = project.viewer().json("/api/entrypoints").await;
    let main = body["files"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["file"] == "src/main.ts")
        .unwrap_or_else(|| panic!("src/main.ts in {}", body["files"]))
        .clone();
    assert_eq!(main["kind"], "file");
    assert!(main["id"].as_str().unwrap().starts_with("file:"));
    assert!(main["calls"].as_i64().unwrap() >= 2);
    assert!(main["reaches"].as_i64().unwrap() >= 2);
    assert!(main["dependents"].is_number());
}

#[tokio::test]
async fn leaves_test_files_out_where_do_i_start_never_means_a_test() {
    let project = fixture();
    let body = project.viewer().json("/api/entrypoints").await;
    for file in body["files"]["items"].as_array().unwrap() {
        assert_eq!(file["test"], false);
    }
    assert!(
        !names(&body["files"]["items"], "/file")
            .iter()
            .any(|f| f == "__tests__/service.test.ts")
    );
    for hub in body["hubs"]["items"].as_array().unwrap() {
        assert_eq!(hub["test"], false);
    }
}

#[tokio::test]
async fn ranks_the_most_depended_on_symbols_as_hubs_with_their_dependent_counts() {
    let project = fixture();
    let body = project.viewer().json("/api/entrypoints").await;
    let hubs = body["hubs"]["items"].as_array().unwrap();
    let hot = hubs
        .iter()
        .find(|h| h["name"] == "hot")
        .expect("hot tops the hubs");
    assert_eq!(hot["dependents"], 500);
    assert_eq!(hubs[0]["name"], "hot");
    let counts = ints(&body["hubs"]["items"], "/dependents");
    assert!(counts.windows(2).all(|w| w[0] >= w[1]));
    for hub in hubs {
        assert!(
            !["file", "import", "export", "parameter"].contains(&hub["kind"].as_str().unwrap())
        );
    }
}

#[tokio::test]
async fn says_a_project_without_routes_is_not_routed_rather_than_failing() {
    let project = fixture();
    let body = project.viewer().json("/api/entrypoints").await;
    assert_eq!(body["routes"]["routed"], false);
    assert_eq!(body["routes"]["items"]["items"], serde_json::json!([]));
    assert_eq!(body["routes"]["routeCount"], 0);
}

#[tokio::test]
async fn honours_limit_and_keeps_every_list_within_it() {
    let project = fixture();
    let viewer = project.viewer();
    let body = viewer.json("/api/entrypoints?limit=1").await;
    assert!(body["files"]["items"].as_array().unwrap().len() <= 1);
    assert_eq!(body["hubs"]["items"].as_array().unwrap().len(), 1);
    assert!(body["hubs"]["total"].as_u64().unwrap() >= 1);
    let bad = viewer.get("/api/entrypoints?limit=0").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["code"], "bad-request");
}

/* ------------------------------------------------------------ /api/nodes -- */

#[tokio::test]
async fn answers_a_batch_of_ids_in_the_order_asked_and_says_which_are_missing() {
    let project = fixture();
    let viewer = project.viewer();
    let cache = viewer.id_of("Cache", Some("class")).await;
    let load = viewer.id_of("load", Some("method")).await;
    let body = viewer
        .json(&format!(
            "/api/nodes?id={}&id={}&id=method%3Anot-a-real-id",
            encode(&load),
            encode(&cache)
        ))
        .await;
    assert_eq!(
        names(&body["items"], "/id"),
        vec![load.clone(), cache.clone()]
    );
    assert_eq!(body["items"][0]["name"], "load");
    assert_eq!(body["items"][1]["name"], "Cache");
    assert_eq!(body["missing"], serde_json::json!(["method:not-a-real-id"]));
    assert!(body["items"][0].get("incoming").is_none());
    assert_eq!(body["items"][0]["file"], "src/service.ts");
}

#[tokio::test]
async fn de_duplicates_ids_rather_than_answering_twice() {
    let project = fixture();
    let viewer = project.viewer();
    let cache = encode(&viewer.id_of("Cache", Some("class")).await);
    let body = viewer
        .json(&format!("/api/nodes?id={cache}&id={cache}"))
        .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn refuses_an_empty_or_oversized_request_with_guidance() {
    let project = fixture();
    let viewer = project.viewer();
    let none = viewer.get("/api/nodes").await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    assert!(none.json()["hint"].as_str().unwrap().contains("id="));
    let ids: Vec<String> = (0..61).map(|i| format!("id=method%3A{i}")).collect();
    let many = viewer.get(&format!("/api/nodes?{}", ids.join("&"))).await;
    assert_eq!(many.status, StatusCode::BAD_REQUEST);
    assert!(
        many.json()["error"]
            .as_str()
            .unwrap()
            .contains("Too many ids")
    );
}

/* -------------------------------------------------------------- no index -- */

#[tokio::test]
async fn answers_with_the_same_guidance_the_cli_prints_not_a_stack_trace() {
    let project = Project::new(&[("src/a.ts", "export const a = 1;\n")]);
    let reply = project.viewer().get("/api/stats").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = reply.json();
    assert_eq!(body["code"], "no-index");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("No CodeGraph index found")
    );
    assert!(body["hint"].as_str().unwrap().contains("codegraph init"));
    assert!(!body["error"].as_str().unwrap().contains("    at "));
}
