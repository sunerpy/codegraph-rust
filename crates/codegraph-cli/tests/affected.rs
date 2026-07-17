//! CLI integration tests for the `affected` subcommand's file-listing output.
//!
//! P3 (Feishu liaison rev15): `affected <file>` counted non-test dependents via
//! `totalDependentsTraversed` but never LISTED them — its only file-listing
//! field, `affectedTests`, held test-file dependents only. These tests lock the
//! additive `affectedFiles` field: the sorted+deduped union of every traversed
//! dependent plus the test files, so `affected` output agrees with
//! `impact`/`audit`. The `affected` command has no `--json` flag; it always
//! emits JSON on stdout.
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
            "codegraph-cli-affected-{label}-{}-{}",
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

fn affected_json(p: &str, file: &str, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["affected", file, "-p", p];
    args.extend_from_slice(extra);
    let (stdout, err, ok) = cli(&args);
    assert!(ok, "affected failed: stdout={stdout} stderr={err}");
    // `affected` always emits JSON on stdout (no --json flag); the logger's
    // one-time init line goes to stderr, so stdout is pure JSON.
    serde_json::from_str(&stdout).expect("affected emits valid JSON on stdout")
}

#[test]
fn impact_and_affected_of_main_scene_list_project_godot() {
    // Given the indexed godot_audit fixture, where project.godot declares
    // `run/main_scene="res://main.tscn"` (an untagged main_scene ref).
    let (_dir, project) = indexed_project("main-scene");
    let p = project.to_str().unwrap();

    // When impact runs on the main scene, project.godot is surfaced as a
    // referrer (P2 — the untagged main_scene reverse lane).
    let (impact_out, err, ok) = cli(&["impact", "main.tscn", "-p", p]);
    assert!(ok, "impact failed: stdout={impact_out} stderr={err}");
    assert!(
        impact_out.contains("project.godot"),
        "impact main.tscn must list project.godot: {impact_out}"
    );

    // And affected agrees at its own entrypoint (both route the shared helper).
    let value = affected_json(p, "main.tscn", &["--depth", "5"]);
    let affected_files = string_array(&value, "affectedFiles");
    assert!(
        affected_files.contains(&"project.godot".to_string()),
        "affected main.tscn must list project.godot: {affected_files:?}"
    );
}

fn string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be a JSON array, got: {value}"))
        .iter()
        .map(|v| v.as_str().expect("array element is a string").to_string())
        .collect()
}

#[test]
fn affected_lists_affected_files() {
    // Given the indexed godot_audit fixture, where main.tscn attaches
    // res://player.gd via ext_resource (so player.gd's dependent is main.tscn).
    let (_dir, project) = indexed_project("lists-files");
    let p = project.to_str().unwrap();

    // When affected runs on the attached script (always JSON on stdout),
    let value = affected_json(p, "player.gd", &["--depth", "5"]);

    // Then affectedFiles is non-empty and contains the dependent scene,
    let affected_files = string_array(&value, "affectedFiles");
    assert!(
        affected_files.contains(&"main.tscn".to_string()),
        "affectedFiles must list the dependent main.tscn, got: {affected_files:?}"
    );

    // and the existing fields are unchanged: affectedTests stays empty (the
    // dependent is not a test file) and totalDependentsTraversed is positive.
    let affected_tests = string_array(&value, "affectedTests");
    assert!(
        affected_tests.is_empty(),
        "affectedTests must stay [] for a non-test dependent, got: {affected_tests:?}"
    );
    let traversed = value["totalDependentsTraversed"]
        .as_u64()
        .expect("totalDependentsTraversed is a number");
    assert!(
        traversed > 0,
        "totalDependentsTraversed must stay positive, got {traversed}"
    );

    // and affectedFiles is sorted + deduped (deterministic output).
    let mut sorted_dedup = affected_files.clone();
    sorted_dedup.sort();
    sorted_dedup.dedup();
    assert_eq!(
        affected_files, sorted_dedup,
        "affectedFiles must be sorted and deduped, got: {affected_files:?}"
    );
}

#[test]
fn affected_files_superset_of_tests() {
    // Given the indexed fixture with a PLANTED test-named scene that also
    // attaches player.gd, so player.gd has BOTH a test dependent and a
    // non-test dependent (main.tscn). The planted scene name matches
    // is_test_file's ".test." rule (a copy of main.tscn, which ext_resource
    // attaches res://player.gd).
    let (_dir, project) = indexed_project("superset");
    fs::copy(project.join("main.tscn"), project.join("player.test.tscn")).unwrap();
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "reindex failed: stdout={out} stderr={err}");

    // When affected runs on the shared script,
    let value = affected_json(p, "player.gd", &["--depth", "5"]);

    // Then affectedTests is non-empty (the planted .test. scene qualifies),
    let affected_tests = string_array(&value, "affectedTests");
    assert!(
        affected_tests
            .iter()
            .any(|f| f.contains(".test.") && f.ends_with(".tscn")),
        "affectedTests must include the planted .test. scene, got: {affected_tests:?}"
    );

    // and affectedFiles is a superset of affectedTests (⊇) AND includes the
    // non-test dependent main.tscn.
    let affected_files = string_array(&value, "affectedFiles");
    for test_file in &affected_tests {
        assert!(
            affected_files.contains(test_file),
            "affectedFiles must be a superset of affectedTests; missing {test_file:?} in {affected_files:?}"
        );
    }
    assert!(
        affected_files.contains(&"main.tscn".to_string()),
        "affectedFiles must also list the non-test dependent main.tscn, got: {affected_files:?}"
    );
}
