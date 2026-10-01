//! Definition-aware CLI output for callers/callees/impact (#1512/#1674).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDir(PathBuf);

/// Parallel tests in this file share a name prefix, and Windows' clock can hand
/// two of them the same nanosecond, so a sequence number keeps each directory
/// apart (a shared one failed `init` with "namespace already exists").
static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-definition-groups-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temp project");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(project: &Path, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .current_dir(project)
        .output()
        .expect("run codegraph");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn json(project: &Path, args: &[&str]) -> serde_json::Value {
    let (stdout, stderr, ok) = run(project, args);
    assert!(ok, "command failed: stdout={stdout} stderr={stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("invalid JSON: {error}; stdout={stdout}; stderr={stderr}"))
}

fn fixture() -> TestDir {
    let dir = TestDir::new();
    fs::create_dir_all(dir.0.join("a")).unwrap();
    fs::create_dir_all(dir.0.join("b")).unwrap();
    fs::write(
        dir.0.join("a/svc.js"),
        "export function alpha() {}\nexport function handle() { alpha(); }\nexport function aMain() { handle(); }\n",
    )
    .unwrap();
    fs::write(
        dir.0.join("b/svc.js"),
        "export function beta() {}\nexport function handle() { beta(); }\nexport function bMain() { handle(); }\n",
    )
    .unwrap();
    fs::write(
        dir.0.join("limit.js"),
        "export function target() {}\nexport function first() { target(); }\nexport function second() { target(); }\n",
    )
    .unwrap();
    let path = dir.0.to_str().unwrap();
    let (stdout, stderr, ok) = run(&dir.0, &["init", path]);
    assert!(ok, "init failed: stdout={stdout} stderr={stderr}");
    dir
}

#[test]
fn callers_and_callees_are_attributed_to_distinct_definitions() {
    let dir = fixture();
    let path = dir.0.to_str().unwrap();

    let callers = json(&dir.0, &["callers", "handle", "-p", path, "--json"]);
    assert_eq!(callers["ambiguous"], true);
    assert_eq!(callers["aggregation"], "union");
    assert_eq!(callers["definitions"].as_array().unwrap().len(), 2);
    assert_eq!(callers["callers"].as_array().unwrap().len(), 2);
    assert_eq!(callers["total"], 2);
    assert_eq!(callers["truncated"], false);

    let definitions = callers["definitions"].as_array().unwrap();
    for definition in definitions {
        let file = definition["definition"]["filePath"].as_str().unwrap();
        let names = definition["callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(definition["callers"][0]["id"].is_string());
        match file {
            "a/svc.js" => assert_eq!(names, vec!["aMain"]),
            "b/svc.js" => assert_eq!(names, vec!["bMain"]),
            other => panic!("unexpected definition file {other}"),
        }
    }

    let callees = json(&dir.0, &["callees", "handle", "-p", path, "--json"]);
    let definitions = callees["definitions"].as_array().unwrap();
    for definition in definitions {
        let file = definition["definition"]["filePath"].as_str().unwrap();
        let names = definition["callees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(definition["callees"][0]["id"].is_string());
        match file {
            "a/svc.js" => assert_eq!(names, vec!["alpha"]),
            "b/svc.js" => assert_eq!(names, vec!["beta"]),
            other => panic!("unexpected definition file {other}"),
        }
    }
}

#[test]
fn file_filter_narrows_and_a_miss_falls_back_visibly() {
    let dir = fixture();
    let path = dir.0.to_str().unwrap();

    let narrowed = json(
        &dir.0,
        &[
            "callers", "handle", "-p", path, "--file", "a/svc.js", "--json",
        ],
    );
    assert_eq!(narrowed["ambiguous"], false);
    assert_eq!(narrowed["filteredOut"], false);
    assert_eq!(narrowed["definitions"].as_array().unwrap().len(), 1);
    assert_eq!(narrowed["callers"][0]["name"], "aMain");

    let fallback = json(
        &dir.0,
        &[
            "callers",
            "handle",
            "-p",
            path,
            "--file",
            "missing.js",
            "--json",
        ],
    );
    assert_eq!(fallback["ambiguous"], true);
    assert_eq!(fallback["filteredOut"], true);
    assert!(
        fallback["note"]
            .as_str()
            .unwrap()
            .contains("showing all definitions")
    );
    assert_eq!(fallback["definitions"].as_array().unwrap().len(), 2);
}

#[test]
fn limits_and_impact_metadata_tell_the_truth() {
    let dir = fixture();
    let path = dir.0.to_str().unwrap();

    let limited = json(
        &dir.0,
        &["callers", "target", "-p", path, "--limit", "1", "--json"],
    );
    assert_eq!(limited["callers"].as_array().unwrap().len(), 1);
    assert_eq!(limited["total"], 2);
    assert_eq!(limited["limit"], 1);
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["definitions"][0]["total"], 2);
    assert_eq!(limited["definitions"][0]["truncated"], true);

    let impact = json(&dir.0, &["impact", "handle", "-p", path, "--json"]);
    assert_eq!(impact["ambiguous"], true);
    assert_eq!(impact["aggregation"], "union");
    assert_eq!(impact["definitions"].as_array().unwrap().len(), 2);
    assert!(impact["nodeCount"].as_u64().unwrap() >= 2);
    for definition in impact["definitions"].as_array().unwrap() {
        assert!(definition["nodeCount"].as_u64().unwrap() >= 1);
        assert!(definition["edges"].is_array());
        assert!(definition["affected"][0]["id"].is_string());
    }
}

#[test]
fn callers_report_each_callers_relationship_kinds() {
    // Upstream #1839 (CLI): a caller that constructs the symbol is marked, in
    // JSON and in text, so it is not read as a plain call.
    let dir = TestDir::new();
    fs::write(
        dir.0.join("widget.ts"),
        "export class Widget {}\n\
         export function make() { return new Widget(); }\n\
         export function use() { return make(); }\n",
    )
    .unwrap();
    let path = dir.0.to_str().unwrap();
    let (stdout, stderr, ok) = run(&dir.0, &["init", path]);
    assert!(ok, "init failed: stdout={stdout} stderr={stderr}");

    let widget = json(&dir.0, &["callers", "Widget", "--path", path, "--json"]);
    assert_eq!(widget["callers"][0]["name"], "make");
    assert_eq!(
        widget["callers"][0]["relationships"],
        serde_json::json!(["instantiates"])
    );
    assert_eq!(
        widget["definitions"][0]["callers"][0]["relationships"],
        serde_json::json!(["instantiates"])
    );
    let make = json(&dir.0, &["callers", "make", "--path", path, "--json"]);
    assert_eq!(
        make["callers"][0]["relationships"],
        serde_json::json!(["calls"])
    );

    let (text, stderr, ok) = run(&dir.0, &["callers", "Widget", "--path", path]);
    assert!(ok, "callers failed: {stderr}");
    assert!(text.contains("make [instantiates]"), "{text}");
    let (text, stderr, ok) = run(&dir.0, &["callers", "make", "--path", path]);
    assert!(ok, "callers failed: {stderr}");
    assert!(text.contains("use\n"), "{text}");
    assert!(!text.contains("[calls]"), "{text}");
}
