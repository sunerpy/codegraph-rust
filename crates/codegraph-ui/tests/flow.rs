//! `GET /api/flow` against a real index — a port of upstream
//! `__tests__/ui-flow-api.test.ts` (`v1.6.1`). The pure parsing and label
//! cases live beside them in `src/api/flow.rs`.

mod support;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use codegraph_graph::flow_boundary::continuations_from;
use codegraph_graph::named_symbol_flow::{NamedSymbolFlowOptions, resolve_named_symbol_flow};
use serde_json::Value;
use support::{Project, Viewer, encode};

fn fixture() -> Project {
    Project::indexed(&[
        // A five-hop chain: bootstrap -> handleRequest -> loadRow -> readRow -> toRow.
        (
            "src/main.ts",
            "import { handleRequest } from './server/handler';

export function bootstrap(): string {
  const banner = 'ready';
  return handleRequest(banner);
}
",
        ),
        (
            "src/server/handler.ts",
            "import { loadRow } from '../db/rows';

export function handleRequest(id: string): string {
  const trimmed = id.trim();
  return loadRow(trimmed);
}

export function orphanHandler(): string {
  return 'nobody calls me';
}
",
        ),
        (
            "src/db/rows.ts",
            "export function loadRow(id: string): string {
  return readRow(id);
}

function readRow(id: string): string {
  return toRow(id);
}

function toRow(id: string): string {
  return id.toUpperCase();
}
",
        ),
        // Two `describeRow` definitions, one in a test file.
        (
            "src/db/describe.ts",
            "import { loadRow } from './rows';

export function describeRow(id: string): string {
  return loadRow(id);
}
",
        ),
        (
            "__tests__/rows.test.ts",
            "export function describeRow(id: string): string {\n  return id;\n}\n",
        ),
        // A registry whose call target is a string key: one literal key, one
        // runtime value.
        (
            "src/router/table.ts",
            "type Handler = (payload: string) => string;

const routerTable: Record<string, Handler> = {};

export function register(key: string, fn: Handler): void {
  routerTable[key] = fn;
}

export function routeSave(payload: string): string {
  return routerTable['save'](payload);
}

export function routeAny(name: string, payload: string): string {
  return routerTable[name](payload);
}

export function beginWork(name: string, payload: string): string {
  return routeAny(name, payload);
}
",
        ),
        (
            "src/router/handlers.ts",
            "import { register } from './table';

export function onSave(payload: string): string {
  return payload;
}

register('save', onSave);
",
        ),
        // A Go interface with one implementation.
        (
            "go/clock.go",
            "package clock

type Clock interface {
\tNow() string
}

type SystemClock struct{}

func (SystemClock) Now() string {
\treturn stamp()
}

func stamp() string {
\treturn \"now\"
}

func Tick(c Clock) string {
\treturn c.Now()
}
",
        ),
    ])
}

async fn flow(viewer: &Viewer, query: &str, expected: StatusCode) -> Value {
    let reply = viewer.get(&format!("/api/flow{query}")).await;
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(reply.status, expected, "{query}: {}", reply.text());
    reply.json()
}

fn names(flow: &Value) -> Vec<String> {
    flow["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["node"]["name"].as_str().unwrap().to_string())
        .collect()
}

fn ids(flow: &Value) -> Vec<String> {
    flow["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["node"]["id"].as_str().unwrap().to_string())
        .collect()
}

/* --------------------------------------------------- a directed question -- */

#[tokio::test]
async fn returns_the_whole_chain_one_hop_per_card() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=toRow",
        StatusCode::OK,
    )
    .await;
    assert_eq!(payload["query"]["kind"], "directed");
    assert_eq!(payload["query"]["from"], "bootstrap");
    assert_eq!(payload["query"]["to"], "toRow");
    assert!(payload["reason"].is_null());
    assert_eq!(payload["flows"].as_array().unwrap().len(), 1);
    assert_eq!(
        names(&payload["flows"][0]),
        ["bootstrap", "handleRequest", "loadRow", "readRow", "toRow"]
    );
    assert_eq!(payload["flows"][0]["label"], "bootstrap → toRow");
}

#[tokio::test]
async fn opens_each_card_at_the_line_that_calls_the_next_one() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=toRow",
        StatusCode::OK,
    )
    .await;
    let hops = payload["flows"][0]["hops"].as_array().unwrap();
    for i in 0..hops.len() - 1 {
        let reference = &hops[i]["callRef"];
        assert!(!reference.is_null(), "hop {i} has a call site");
        assert_eq!(reference["name"], hops[i + 1]["node"]["name"]);
        assert_eq!(reference["targetId"], hops[i + 1]["node"]["id"]);
        assert_eq!(reference["backwards"], false);
        let line = reference["line"].as_i64().unwrap();
        let from = hops[i]["source"]["from"].as_i64().unwrap();
        assert!(line >= from && line <= hops[i]["source"]["to"].as_i64().unwrap());
        let text = hops[i]["source"]["lines"][(line - from) as usize]
            .as_str()
            .unwrap();
        assert!(
            text.contains(hops[i + 1]["node"]["name"].as_str().unwrap()),
            "{text}"
        );
    }
    let last = hops.last().unwrap();
    assert!(last["callRef"].is_null());
    let line = last["node"]["line"].as_i64().unwrap();
    assert!(last["source"]["from"].as_i64().unwrap() <= line);
    assert!(last["source"]["to"].as_i64().unwrap() >= line);
}

#[tokio::test]
async fn carries_the_edge_on_every_hop_but_the_first_with_its_line() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=toRow",
        StatusCode::OK,
    )
    .await;
    let hops = payload["flows"][0]["hops"].as_array().unwrap();
    assert!(hops[0]["edge"].is_null());
    for i in 1..hops.len() {
        let edge = &hops[i]["edge"];
        assert_eq!(edge["kind"], "calls");
        assert_eq!(edge["label"], "calls");
        assert_eq!(edge["upward"], false);
        assert_eq!(edge["synthesized"], false);
        assert_eq!(edge["line"], hops[i - 1]["callRef"]["line"]);
    }
}

#[tokio::test]
async fn highlights_each_card_with_real_source_never_a_drifted_slice() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=toRow",
        StatusCode::OK,
    )
    .await;
    for hop in payload["flows"][0]["hops"].as_array().unwrap() {
        let source = &hop["source"];
        assert_eq!(source["drift"], false);
        let lines = source["lines"].as_array().unwrap();
        assert!(!lines.is_empty());
        assert_eq!(
            lines.len() as i64,
            source["to"].as_i64().unwrap() - source["from"].as_i64().unwrap() + 1
        );
        assert_eq!(
            source["highlight"]["lines"].as_array().unwrap().len(),
            lines.len()
        );
    }
}

#[tokio::test]
async fn answers_not_connected_as_an_ordinary_answer_with_a_reason() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=orphanHandler",
        StatusCode::OK,
    )
    .await;
    assert_eq!(payload["flows"], serde_json::json!([]));
    let reason = payload["reason"].as_str().unwrap();
    assert!(
        reason.contains("No chain of calls reaches orphanHandler"),
        "{reason}"
    );
    assert!(reason.contains("dynamic dispatch"));
    assert_eq!(payload["unresolved"], serde_json::json!([]));
}

#[tokio::test]
async fn says_which_names_matched_nothing_rather_than_blaming_the_path() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=thisNameIsNotHere",
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        payload["unresolved"],
        serde_json::json!(["thisNameIsNotHere"])
    );
    assert!(
        payload["reason"]
            .as_str()
            .unwrap()
            .contains("thisNameIsNotHere names nothing")
    );
}

#[tokio::test]
async fn walks_past_an_overload_in_a_test_file_and_reports_the_ambiguity() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=describeRow&to=toRow",
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        names(&payload["flows"][0]),
        ["describeRow", "loadRow", "readRow", "toRow"]
    );
    let ambiguity = payload["ambiguous"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["token"] == "describeRow")
        .expect("the describeRow ambiguity");
    assert_eq!(ambiguity["chosen"]["file"], "src/db/describe.ts");
    assert!(
        ambiguity["others"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["file"] == "__tests__/rows.test.ts")
    );
}

/* -------------------------------------------------- where the graph stops -- */

#[tokio::test]
async fn caps_a_keyed_dispatch_with_its_form_its_key_and_a_candidate_target() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=routeSave&to=onSave",
        StatusCode::OK,
    )
    .await;
    assert!(
        payload["reason"]
            .as_str()
            .unwrap()
            .contains("No chain of calls reaches onSave")
    );
    let flow = &payload["flows"][0];
    assert_eq!(flow["partial"], true);
    assert_eq!(names(flow), ["routeSave"]);
    assert_eq!(flow["label"], "routeSave → stops here");
    let boundary = &flow["boundary"];
    assert_eq!(boundary["node"]["name"], "routeSave");
    let site = &boundary["sites"][0];
    assert_eq!(site["form"], "computed-call");
    assert_eq!(site["label"], "computed member call");
    assert_eq!(site["key"], "save");
    assert!(site["line"].as_i64().unwrap() > boundary["node"]["line"].as_i64().unwrap());
    let candidate = site["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["display"] == "onSave")
        .unwrap_or_else(|| panic!("onSave among {}", site["candidates"]));
    // The reader named it, so the cap says so rather than presenting it as new.
    assert_eq!(candidate["named"], true);
    assert!(
        boundary["missed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "onSave")
    );
}

#[tokio::test]
async fn opens_the_card_at_the_dispatch_line_with_real_source_around_it() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=routeSave&to=onSave",
        StatusCode::OK,
    )
    .await;
    let flow = &payload["flows"][0];
    let line = flow["boundary"]["sites"][0]["line"].as_i64().unwrap();
    let source = &flow["hops"][0]["source"];
    assert_eq!(source["drift"], false);
    assert!(source["from"].as_i64().unwrap() <= line && source["to"].as_i64().unwrap() >= line);
    let text: Vec<&str> = source["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(text.join("\n").contains("routerTable['save']"));
}

#[tokio::test]
async fn claims_no_candidates_when_the_key_is_a_runtime_value() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=routeAny&to=onSave",
        StatusCode::OK,
    )
    .await;
    let site = &payload["flows"][0]["boundary"]["sites"][0];
    assert_eq!(site["form"], "computed-call");
    assert!(site["key"].is_null());
    assert_eq!(site["candidates"], serde_json::json!([]));
    assert!(site["candidateNote"].is_null());
}

#[tokio::test]
async fn caps_a_chain_that_connects_but_never_reaches_everything_it_was_asked_about() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?symbols=beginWork,routeAny,onSave",
        StatusCode::OK,
    )
    .await;
    let flow = &payload["flows"][0];
    assert_eq!(flow["partial"], false);
    assert_eq!(names(flow), ["beginWork", "routeAny"]);
    assert_eq!(flow["boundary"]["node"]["name"], "routeAny");
    assert_eq!(flow["boundary"]["sites"][0]["form"], "computed-call");
    let missed: Vec<&str> = flow["boundary"]["missed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(missed, ["onSave"]);
    let last = &flow["hops"].as_array().unwrap().last().unwrap()["source"];
    let stop = flow["boundary"]["sites"][0]["line"].as_i64().unwrap();
    assert!(last["from"].as_i64().unwrap() <= stop && last["to"].as_i64().unwrap() >= stop);
}

#[tokio::test]
async fn never_caps_a_flow_that_reaches_what_it_was_asked_for() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=toRow",
        StatusCode::OK,
    )
    .await;
    assert!(payload["flows"][0]["boundary"].is_null());
    assert_eq!(payload["flows"][0]["partial"], false);
}

#[tokio::test]
async fn stays_silent_when_nothing_connects_and_no_dispatch_site_explains_it() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=bootstrap&to=orphanHandler",
        StatusCode::OK,
    )
    .await;
    assert_eq!(payload["flows"], serde_json::json!([]));
}

#[tokio::test]
async fn counts_the_calls_the_path_did_not_need_and_lists_them() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?symbols=beginWork,routeAny,onSave",
        StatusCode::OK,
    )
    .await;
    let boundary = &payload["flows"][0]["boundary"];
    for list in [&boundary["further"], &boundary["uncertain"]] {
        assert_eq!(
            list["shown"].as_u64().unwrap() as usize,
            list["items"].as_array().unwrap().len()
        );
        assert!(list["total"].as_u64() >= list["shown"].as_u64());
    }
}

/* -------------------------------------- the end cap and explore agree ----- */

#[tokio::test]
async fn names_the_same_site_the_same_key_and_the_same_candidate() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?from=routeSave&to=onSave",
        StatusCode::OK,
    )
    .await;
    let site = payload["flows"][0]["boundary"]["sites"][0].clone();
    let root = project.root().to_path_buf();
    let text = tokio::task::spawn_blocking(move || {
        let engine = codegraph_mcp::CodeGraphEngine::open(&root).expect("engine");
        let result = engine.execute(
            "codegraph_explore",
            &serde_json::json!({ "query": "routeSave onSave" }),
        );
        result.content[0].text.clone()
    })
    .await
    .unwrap();
    // Both name the same dispatch site, key and candidate, so the strip and
    // the MCP answer side by side never disagree about where a path ends.
    assert!(text.contains("Dynamic boundaries"), "{text}");
    assert!(text.contains(site["label"].as_str().unwrap()));
    assert!(
        text.contains(&format!("src/router/table.ts:{}", site["line"])),
        "{text}"
    );
    assert!(text.contains(&format!(
        "candidates for key `{}`",
        site["key"].as_str().unwrap()
    )));
    for candidate in site["candidates"].as_array().unwrap() {
        assert!(text.contains(candidate["display"].as_str().unwrap()));
    }
}

#[tokio::test]
async fn splits_a_symbols_outgoing_calls_into_the_sure_and_the_unfollowed() {
    let project = fixture();
    let paths = codegraph_core::IndexPaths::resolve(project.root(), None).unwrap();
    let store = codegraph_store::Store::open_for_read(
        &paths,
        Instant::now() + Duration::from_secs(30),
        || false,
    )
    .expect("read the index");
    let node = store.nodes_by_name("handleRequest").unwrap().remove(0);
    let all = continuations_from(&store, &node, &HashSet::new()).unwrap();
    assert!(all.resolved.iter().any(|c| c.node.name == "loadRow"));
    assert!(
        all.uncertain
            .iter()
            .all(|c| c.confidence.unwrap_or(1.0) < 0.6)
    );
    let target = all.resolved[0].node.id.clone();
    let rest = continuations_from(&store, &node, &[target.clone()].into_iter().collect()).unwrap();
    assert!(!rest.resolved.iter().any(|c| c.node.id == target));
}

/* ------------------------------------------------------- a synthesized hop -- */

#[tokio::test]
async fn an_interface_dispatch_the_graph_does_not_bridge_ends_where_the_static_graph_does() {
    // KEEP-RUST: upstream's synthesis stage adds an interface-impl `calls` edge
    // for Go's implicit satisfaction and the strip draws it as a dashed `via …`
    // hop (its case "draws the interface bridge as a dashed hop"). The Rust
    // port has no edge synthesis stage, so the same question stops at the
    // interface method — and says so, rather than inventing the hop. The
    // dashed-hop labels are covered by `flow_edge_label`'s own cases.
    let project = fixture();
    let payload = flow(&project.viewer(), "?from=Tick&to=stamp", StatusCode::OK).await;
    assert_eq!(payload["flows"], serde_json::json!([]), "{payload}");
    assert!(
        payload["reason"]
            .as_str()
            .unwrap()
            .contains("dynamic dispatch")
    );
    assert_eq!(payload["unresolved"], serde_json::json!([]));
}

/* ---------------------------------------------------------- explore parity -- */

#[tokio::test]
async fn answers_a_symbols_question_with_the_chain_the_explore_search_finds() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?symbols=bootstrap,loadRow,toRow",
        StatusCode::OK,
    )
    .await;
    assert_eq!(payload["query"]["kind"], "symbols");
    assert!(!payload["flows"].as_array().unwrap().is_empty());
    // The endpoint has no path finder of its own: the engine's answers the same.
    let paths = codegraph_core::IndexPaths::resolve(project.root(), None).unwrap();
    let store = codegraph_store::Store::open_for_read(
        &paths,
        Instant::now() + Duration::from_secs(30),
        || false,
    )
    .expect("read the index");
    let engine = resolve_named_symbol_flow(
        &store,
        "bootstrap,loadRow,toRow",
        &NamedSymbolFlowOptions::default(),
    );
    let steps: Vec<String> = engine.chains[0]
        .steps
        .iter()
        .map(|s| s.node.id.clone())
        .collect();
    assert_eq!(steps, ids(&payload["flows"][0]));
}

/* --------------------------------------------------- a trail read as a flow -- */

#[tokio::test]
async fn draws_the_hops_it_was_given_finding_the_edge_that_already_joins_them() {
    let project = fixture();
    let viewer = project.viewer();
    let forward = flow(&viewer, "?from=bootstrap&to=toRow", StatusCode::OK).await;
    let hop_ids = ids(&forward["flows"][0]);
    let query: Vec<String> = hop_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            format!(
                "hop={}",
                encode(&format!("{}{id}", if i == 0 { 's' } else { 'd' }))
            )
        })
        .collect();
    let payload = flow(&viewer, &format!("?{}", query.join("&")), StatusCode::OK).await;
    assert_eq!(payload["query"]["kind"], "trail");
    assert_eq!(ids(&payload["flows"][0]), hop_ids);
    assert_eq!(payload["flows"][0]["hops"][1]["edge"]["kind"], "calls");
    assert_eq!(payload["flows"][0]["hops"][1]["edge"]["upward"], false);
}

#[tokio::test]
async fn reads_a_trail_walked_backwards_as_caller_hops_opened_at_the_calling_line() {
    let project = fixture();
    let viewer = project.viewer();
    let forward = flow(&viewer, "?from=bootstrap&to=toRow", StatusCode::OK).await;
    let mut hop_ids = ids(&forward["flows"][0]);
    hop_ids.reverse();
    let query: Vec<String> = hop_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            format!(
                "hop={}",
                encode(&format!("{}{id}", if i == 0 { 's' } else { 'u' }))
            )
        })
        .collect();
    let payload = flow(&viewer, &format!("?{}", query.join("&")), StatusCode::OK).await;
    let hops = payload["flows"][0]["hops"].as_array().unwrap();
    assert_eq!(ids(&payload["flows"][0]), hop_ids);
    for i in 1..hops.len() {
        assert_eq!(hops[i]["edge"]["upward"], true);
        assert_eq!(hops[i]["edge"]["label"], "called by");
        assert_eq!(hops[i]["callRef"]["backwards"], true);
        assert_eq!(hops[i]["callRef"]["name"], hops[i - 1]["node"]["name"]);
        assert_eq!(hops[i]["callRef"]["line"], hops[i]["edge"]["line"]);
    }
    assert!(hops[0]["callRef"].is_null());
}

#[tokio::test]
async fn says_so_when_the_ids_on_a_trail_are_no_longer_in_the_index() {
    let project = fixture();
    let payload = flow(
        &project.viewer(),
        "?hop=smethod%3Agone&hop=dmethod%3Aalso-gone",
        StatusCode::OK,
    )
    .await;
    assert_eq!(payload["flows"], serde_json::json!([]));
    assert_eq!(
        payload["unresolved"],
        serde_json::json!(["method:gone", "method:also-gone"])
    );
    assert!(
        payload["reason"]
            .as_str()
            .unwrap()
            .contains("still in the index")
    );
}

/* ---------------------------------------------------------------- refusals -- */

#[tokio::test]
async fn answers_json_not_text_when_the_question_is_malformed() {
    let project = fixture();
    let payload = flow(&project.viewer(), "", StatusCode::BAD_REQUEST).await;
    assert_eq!(payload["code"], "bad-request");
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("No flow was asked for")
    );
    assert!(payload["hint"].as_str().unwrap().contains("?from="));
}

#[tokio::test]
async fn caps_the_number_of_trail_hops_it_will_read_at_what_the_trail_store_saves_1976() {
    let project = fixture();
    let viewer = project.viewer();
    let hops = |n: usize| {
        (0..n)
            .map(|i| format!("hop=s{i}xx"))
            .collect::<Vec<_>>()
            .join("&")
    };
    assert_eq!(
        viewer.get(&format!("/api/flow?{}", hops(64))).await.status,
        StatusCode::OK
    );
    let payload = flow(&viewer, &format!("?{}", hops(65)), StatusCode::BAD_REQUEST).await;
    assert_eq!(payload["code"], "bad-request");
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("longer than this endpoint reads (64)")
    );
}

#[tokio::test]
async fn is_listed_on_the_api_index() {
    let project = fixture();
    let index = project.viewer().json("/api").await;
    let entry = index["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "/api/flow")
        .expect("/api/flow listed");
    let params = entry["params"].as_array().unwrap();
    assert!(params.contains(&Value::from("from")) && params.contains(&Value::from("hop")));
}
