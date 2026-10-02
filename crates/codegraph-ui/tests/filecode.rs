//! `GET /api/filecode/<path>` — a port of upstream
//! `__tests__/ui-filecode-api.test.ts` (`v1.6.1`).

mod support;

use axum::http::StatusCode;
use serde_json::Value;
use support::{Project, Viewer};

const REPORT_TS: &str = "import { widen } from './widen';

export function format(value: string): string {
  return value.trim();
}

export function render(a: string, b: string): string {
  const left = format(a);
  const right = format(b);
  return left + right;
}

export function summarise(rows: string[]): string {
  const head = format(rows[0] ?? '');
  console.log(head);
  return widen(head);
}

render('a', 'b');
";

const WIDEN_TS: &str = "export function widen(text: string): string {
  return text + '  ';
}
";

fn fixture() -> Project {
    Project::indexed(&[
        ("src/report.ts", REPORT_TS),
        ("src/widen.ts", WIDEN_TS),
        // Nothing in it reaches anything.
        ("src/quiet.ts", "export const NAME = 'quiet';\n"),
    ])
}

async fn code(viewer: &Viewer, file: &str, expected: StatusCode) -> Value {
    let reply = viewer.get(&format!("/api/filecode/{file}")).await;
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(reply.status, expected, "{file}: {}", reply.text());
    reply.json()
}

/// Rows as `caller -> callee`, the way the rail reads.
fn pairs(payload: &Value) -> Vec<String> {
    let name_of = |id: &Value| {
        payload["outline"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == *id)
            .map(|e| e["name"].as_str().unwrap().to_string())
            .unwrap_or_else(|| "file".to_string())
    };
    payload["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{} -> {}",
                name_of(&c["ownerId"]),
                c["relation"]["node"]["name"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn describes_the_file_and_its_length_which_is_the_views_layout() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    assert_eq!(payload["file"]["path"], "src/report.ts");
    assert_eq!(payload["file"]["language"], "typescript");
    assert_eq!(payload["file"]["id"], "file:src/report.ts");
    assert_eq!(payload["drift"], false);
    let on_disk = REPORT_TS.trim_end_matches('\n').split('\n').count();
    assert_eq!(payload["file"]["totalLines"], on_disk);
}

#[tokio::test]
async fn returns_the_same_outline_rows_the_file_view_draws() {
    let project = fixture();
    let viewer = project.viewer();
    let code = code(&viewer, "src/report.ts", StatusCode::OK).await;
    let file = viewer.json("/api/file/src/report.ts").await;
    assert_eq!(code["outline"]["total"], file["outline"]["total"]);
    let names = |v: &Value| -> Vec<String> {
        v["outline"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&code), names(&file));
    for entry in code["outline"]["items"].as_array().unwrap() {
        assert!(entry["line"].as_i64().unwrap() > 0);
        assert!(entry["endLine"].as_i64().unwrap() >= entry["line"].as_i64().unwrap());
    }
}

#[tokio::test]
async fn groups_by_the_pair_so_one_callee_reached_from_two_functions_is_two_rows() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let rows = pairs(&payload);
    assert!(rows.contains(&"render -> format".to_string()), "{rows:?}");
    assert!(
        rows.contains(&"summarise -> format".to_string()),
        "{rows:?}"
    );
    let render = payload["outline"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "render")
        .unwrap()["id"]
        .clone();
    let row = payload["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["relation"]["node"]["name"] == "format" && c["ownerId"] == render)
        .unwrap()
        .clone();
    let lines = row["relation"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].as_i64() < lines[1].as_i64());
}

#[tokio::test]
async fn rows_are_in_call_site_order_the_only_ordering_the_screen_has() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let firsts: Vec<i64> = payload["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["relation"]["lines"][0].as_i64().unwrap_or(i64::MAX))
        .collect();
    let mut sorted = firsts.clone();
    sorted.sort();
    assert_eq!(firsts, sorted);
}

#[tokio::test]
async fn gives_top_level_code_an_owner_so_a_statement_outside_every_definition_has_a_port() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let top: Vec<&str> = payload["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["ownerId"] == payload["file"]["id"])
        .map(|c| c["relation"]["node"]["name"].as_str().unwrap())
        .collect();
    assert!(top.contains(&"render"), "{top:?}");
}

#[tokio::test]
async fn counts_exactly_the_arcs_the_payload_can_draw() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let mut arcs = 0;
    for call in payload["calls"]["items"].as_array().unwrap() {
        if call["relation"]["node"]["file"] != payload["file"]["path"] {
            continue;
        }
        let target = call["relation"]["node"]["line"].as_i64();
        arcs += call["relation"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l.as_i64() != target)
            .count();
    }
    assert_eq!(payload["intraFileCalls"].as_u64().unwrap() as usize, arcs);
    // render x2, summarise x1, top-level render x1.
    assert!(arcs >= 4, "{arcs}");
}

#[tokio::test]
async fn does_not_count_a_cross_file_call_as_an_arc() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let widen = payload["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["relation"]["node"]["name"] == "widen")
        .expect("summarise -> widen")
        .clone();
    assert_eq!(widen["relation"]["node"]["file"], "src/widen.ts");
}

#[tokio::test]
async fn returns_references_that_resolved_to_nothing_with_a_line_and_a_plain_name() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    let outside = payload["outside"]["items"].as_array().unwrap();
    let names: Vec<&str> = outside
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"log"), "{names:?}");
    for reference in outside {
        assert!(reference["line"].as_i64().unwrap() > 0);
        let name = reference["name"].as_str().unwrap();
        let mut chars = name.chars();
        assert!(
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        );
        assert!(chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$'));
    }
    assert_eq!(
        payload["outside"]["total"].as_u64().unwrap() as usize,
        outside.len()
    );
    assert!(payload["outside"]["shown"].as_u64().unwrap() <= 3000);
}

#[tokio::test]
async fn answers_for_a_file_that_reaches_nothing_without_inventing_rows() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/quiet.ts", StatusCode::OK).await;
    assert_eq!(payload["calls"]["total"], 0);
    assert_eq!(payload["calls"]["items"], serde_json::json!([]));
    assert_eq!(payload["intraFileCalls"], 0);
    assert_eq!(payload["file"]["totalLines"], 1);
}

#[tokio::test]
async fn every_capped_list_still_reports_its_real_total() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/report.ts", StatusCode::OK).await;
    for key in ["outline", "calls", "outside"] {
        let list = &payload[key];
        assert_eq!(
            list["shown"].as_u64().unwrap() as usize,
            list["items"].as_array().unwrap().len()
        );
        assert!(list["total"].as_u64() >= list["shown"].as_u64());
        assert_eq!(
            list["truncated"],
            list["shown"].as_u64() < list["total"].as_u64()
        );
    }
    assert!(payload["calls"]["shown"].as_u64().unwrap() <= 2000);
}

#[tokio::test]
async fn refuses_a_path_outside_the_project_before_it_looks_in_the_index() {
    let project = fixture();
    let reply = project.viewer().get("/api/filecode//etc/passwd").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["code"], "refused");
}

#[tokio::test]
async fn answers_404_for_a_file_that_is_fine_but_not_indexed() {
    let project = fixture();
    let payload = code(&project.viewer(), "src/nope.ts", StatusCode::NOT_FOUND).await;
    assert_eq!(payload["code"], "not-found");
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("not in this CodeGraph index")
    );
}

#[tokio::test]
async fn says_what_the_endpoint_wants_when_given_no_path() {
    let project = fixture();
    let reply = project.viewer().get("/api/filecode").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("/api/filecode/<path>")
    );
}

#[tokio::test]
async fn is_listed_on_the_api_index() {
    let project = fixture();
    let index = project.viewer().json("/api").await;
    let endpoints = index["endpoints"].as_array().unwrap();
    assert!(
        endpoints
            .iter()
            .any(|e| e["path"] == "/api/filecode/<path>")
    );
    assert!(endpoints.iter().any(|e| e["path"] == "/api/file/<path>"));
}

#[tokio::test]
async fn flags_a_file_that_changed_on_disk_and_withholds_its_length() {
    let project = fixture();
    project.write("src/widen.ts", &format!("// a new first line\n{WIDEN_TS}"));
    let payload = code(&project.viewer(), "src/widen.ts", StatusCode::OK).await;
    assert_eq!(payload["drift"], true);
    assert!(
        payload["reason"]
            .as_str()
            .unwrap()
            .contains("changed on disk")
    );
    assert!(payload["outline"]["total"].as_u64().unwrap() > 0);
}
