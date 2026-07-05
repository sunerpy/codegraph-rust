//! CLI integration tests for the `files` subcommand's `--filter` (path prefix)
//! and `--language` filters, plus the symbol-count display reconciliation (P2):
//! `.tscn`/`.tres` files must show their live `nodes`-table count, not a stale 0.
//!
//! Drives the real `codegraph` binary against the committed
//! `tests/fixtures/godot_audit/` project.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/godot_audit")
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
            "codegraph-cli-files-{label}-{}-{}",
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
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn indexed_project(label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path().join("godot_audit");
    copy_tree(&fixture(), &project);
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");
    (dir, project)
}

fn files_json(p: &str, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["files", "-p", p, "--json"];
    args.extend_from_slice(extra);
    let (stdout, err, ok) = cli(&args);
    assert!(ok, "files failed: stderr={err}");
    serde_json::from_str(&stdout).expect("files emits valid JSON")
}

fn paths_of(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().expect("path").to_string())
        .collect()
}

#[test]
fn language_filter_keeps_only_matching_gdscript() {
    // Given the indexed godot_audit fixture,
    let (_dir, project) = indexed_project("lang-gd");
    let p = project.to_str().unwrap();
    // When files runs with --language gdscript,
    let value = files_json(p, &["--language", "gdscript"]);
    // Then only .gd files remain.
    let paths = paths_of(&value);
    assert!(
        !paths.is_empty() && paths.iter().all(|path| path.ends_with(".gd")),
        "expected only .gd files, got: {paths:?}"
    );
}

#[test]
fn language_filter_keeps_only_matching_godot_resource() {
    // Given the indexed fixture,
    let (_dir, project) = indexed_project("lang-tres");
    let p = project.to_str().unwrap();
    // When files runs with --language godot_resource,
    let value = files_json(p, &["--language", "godot_resource"]);
    // Then only .tres files remain.
    let paths = paths_of(&value);
    assert!(
        !paths.is_empty() && paths.iter().all(|path| path.ends_with(".tres")),
        "expected only .tres files, got: {paths:?}"
    );
}

#[test]
fn unknown_language_yields_empty_result_and_no_stderr_hint() {
    // Given the indexed fixture,
    let (_dir, project) = indexed_project("lang-unknown");
    let p = project.to_str().unwrap();
    // When files runs with a language no file uses,
    let (stdout, stderr, ok) = cli(&["files", "-p", p, "--language", "nosuchlang", "--json"]);
    assert!(ok, "files must still succeed: stderr={stderr}");
    // Then the JSON result is empty and no unknown-language hint is printed to
    // stderr (the one-time "logger initialized" line is not a hint).
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        value.as_array().expect("array").is_empty(),
        "unknown language must yield an empty result, got: {value}"
    );
    assert!(
        !stderr.contains("nosuchlang") && !stderr.to_lowercase().contains("language"),
        "unknown language must emit no stderr hint, got: {stderr:?}"
    );
}

#[test]
fn filter_is_a_path_prefix_not_a_language() {
    // Given the indexed fixture with a planted subdir file,
    let (_dir, project) = indexed_project("filter-prefix");
    let sub = project.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::copy(project.join("orphan.tres"), sub.join("nested.tres")).unwrap();
    let p = project.to_str().unwrap();
    let (_o, _e, ok) = cli(&["index", "--force", p]);
    assert!(ok, "reindex failed");
    // When files runs with --filter sub,
    let value = files_json(p, &["--filter", "sub"]);
    // Then only files under sub/ remain (prefix semantics).
    let paths = paths_of(&value);
    assert!(
        !paths.is_empty() && paths.iter().all(|path| path.starts_with("sub/")),
        "expected only files under sub/, got: {paths:?}"
    );
}

#[test]
fn scene_symbol_count_matches_query_not_zero() {
    // Given the indexed fixture whose main.tscn has at least one scene node,
    let (_dir, project) = indexed_project("symbol-count");
    let p = project.to_str().unwrap();
    // When we read main.tscn's displayed symbol count via files --json,
    let value = files_json(p, &[]);
    let scene = value
        .as_array()
        .expect("array")
        .iter()
        .find(|f| f["path"].as_str() == Some("main.tscn"))
        .expect("main.tscn in files output");
    let displayed = scene["nodeCount"].as_i64().expect("nodeCount");
    // Then the displayed count is non-zero — the bug showed 0 for scene files
    // even though the nodes table holds the scene's marker nodes (which query
    // and search also read).
    assert!(
        displayed > 0,
        "main.tscn must show a non-zero symbol count, got {displayed}"
    );
}
