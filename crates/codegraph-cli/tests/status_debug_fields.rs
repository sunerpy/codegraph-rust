//! CLI integration test for the enriched `status --json` diagnostics surface.
//!
//! Drives the real `codegraph` binary against a copy of the committed
//! `crates/codegraph-bench/fixtures/mini` project and asserts the new
//! path-tracing fields (`dbPath`, `dbExists`, `daemonRunning`,
//! `daemonPidPath`, `daemonSocketPath`, `daemonLogPath`) are present in BOTH
//! the initialized and uninitialized JSON.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn mini_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .join("crates/codegraph-bench/fixtures/mini")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-status-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn assert_debug_fields(v: &Value) {
    for key in [
        "dbPath",
        "dbExists",
        "daemonRunning",
        "daemonPidPath",
        "daemonSocketPath",
        "daemonLogPath",
    ] {
        assert!(
            v.get(key).is_some(),
            "status --json must expose `{key}`: {v}"
        );
    }
    assert!(
        v["dbPath"]
            .as_str()
            .is_some_and(|p| p.ends_with("codegraph.db")),
        "dbPath must point at codegraph.db: {v}"
    );
    assert!(
        v["daemonRunning"].is_boolean(),
        "daemonRunning must be a bool: {v}"
    );
}

#[test]
fn status_json_exposes_debug_fields_uninitialized() {
    // GIVEN an unindexed directory.
    let dir = TestDir::new("uninit");
    let p = dir.path().to_str().unwrap();

    // WHEN `status --json` runs against it.
    let (out, err, ok) = cli(&["status", "--json", p]);
    assert!(ok, "status failed: stdout={out} stderr={err}");
    let v: Value = serde_json::from_str(out.trim()).expect("valid JSON");

    // THEN it reports uninitialized and still exposes the diagnostics fields.
    assert_eq!(
        v["initialized"],
        Value::Bool(false),
        "must be uninitialized"
    );
    assert_eq!(v["dbExists"], Value::Bool(false), "db must not exist yet");
    assert_debug_fields(&v);
}

#[test]
fn status_json_exposes_debug_fields_initialized() {
    // GIVEN an indexed copy of the mini fixture.
    let dir = TestDir::new("init");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    // WHEN `status --json` runs against the indexed project.
    let (out, err, ok) = cli(&["status", "--json", p]);
    assert!(ok, "status failed: stdout={out} stderr={err}");
    let v: Value = serde_json::from_str(out.trim()).expect("valid JSON");

    // THEN it reports initialized with an existing db and the diagnostics fields.
    assert_eq!(v["initialized"], Value::Bool(true), "must be initialized");
    assert_eq!(v["dbExists"], Value::Bool(true), "db must exist post-init");
    assert_debug_fields(&v);
    // #1187: a healthy index must NOT carry the partial flag.
    assert!(
        v["index"].get("partial").is_none(),
        "a healthy index must omit index.partial: {v}"
    );
}

#[test]
fn status_json_flags_partial_only_when_marker_set() {
    use codegraph_store::Store;

    // GIVEN an indexed project with the #1187 incomplete-resolution marker set,
    // simulating an interrupted resolution pass.
    let dir = TestDir::new("partial");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let db = project.join(".codegraph").join("codegraph.db");
    {
        let store = Store::open(&db).unwrap();
        store.set_resolution_incomplete().unwrap();
    }

    // WHEN `status --json` runs.
    let (out, err, ok) = cli(&["status", "--json", p]);
    assert!(ok, "status failed: stdout={out} stderr={err}");
    let v: Value = serde_json::from_str(out.trim()).expect("valid JSON");

    // THEN index.partial is true.
    assert_eq!(
        v["index"]["partial"],
        Value::Bool(true),
        "index.partial must be true when the marker is set: {v}"
    );

    // AND after a heal sync the flag is gone again.
    let (out, err, ok) = cli(&["sync", p]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["status", "--json", p]);
    assert!(ok, "status failed: stdout={out} stderr={err}");
    let v: Value = serde_json::from_str(out.trim()).expect("valid JSON");
    assert!(
        v["index"].get("partial").is_none(),
        "a healed index must drop index.partial: {v}"
    );
}
