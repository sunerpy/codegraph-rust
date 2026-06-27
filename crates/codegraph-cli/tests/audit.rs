//! CLI integration test for the read-only `audit` subcommand.
//!
//! Drives the real `codegraph` binary against the committed
//! `tests/fixtures/godot_audit/` project: indexes it into a temp store, runs
//! `audit --orphans --dangling --json` and asserts the planted orphan +
//! dangling, then asserts plain `check` stdout still byte-matches the committed
//! B9 baseline (the lock proving the new subcommand left `check` untouched).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/godot_audit")
}

fn snapshot(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(name),
    )
    .expect("read committed snapshot")
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
            "codegraph-cli-audit-{label}-{}-{}",
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

#[test]
fn audit_reports_planted_orphan_and_dangling_as_json() {
    // Given the committed godot_audit fixture indexed into a temp store,
    let (_dir, project) = indexed_project("json");
    let p = project.to_str().unwrap();

    // When audit runs with --orphans --dangling --json,
    let (stdout, err, ok) = cli(&["audit", "--orphans", "--dangling", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("audit emits valid JSON");

    // Then orphan.tres is reported orphan and referenced.tres is not.
    let orphan_paths: Vec<&str> = value["orphans"]
        .as_array()
        .expect("orphans array")
        .iter()
        .map(|o| o["filePath"].as_str().expect("filePath"))
        .collect();
    assert!(
        orphan_paths.contains(&"orphan.tres"),
        "orphan.tres must be reported orphan, got: {orphan_paths:?}"
    );
    assert!(
        !orphan_paths.contains(&"referenced.tres"),
        "referenced.tres is referenced by data.tres and must NOT be orphan, got: {orphan_paths:?}"
    );

    // And the missing-on-disk references are reported dangling.
    let dangling_targets: Vec<&str> = value["dangling"]
        .as_array()
        .expect("dangling array")
        .iter()
        .map(|d| d["targetPath"].as_str().expect("targetPath"))
        .collect();
    assert!(
        dangling_targets.contains(&"missing/ghost.tres"),
        "missing/ghost.tres must be dangling, got: {dangling_targets:?}"
    );
}

#[test]
fn audit_requires_at_least_one_mode() {
    // Given the indexed fixture,
    let (_dir, project) = indexed_project("no-mode");
    let p = project.to_str().unwrap();
    // When audit runs with no mode flag,
    let (_out, _err, ok) = cli(&["audit", "-p", p]);
    // Then it fails.
    assert!(!ok, "audit with no mode flag must fail");
}

#[test]
fn check_stdout_is_byte_identical_to_the_committed_baseline() {
    // Given the indexed fixture (after the audit subcommand was added),
    let (_dir, project) = indexed_project("check-lock");
    let p = project.to_str().unwrap();

    // When plain `check` and `check --json` run,
    let (text, err, ok) = cli(&["check", "-p", p]);
    assert!(ok, "check failed: stderr={err}");
    let (json, err, ok) = cli(&["check", "-p", p, "--json"]);
    assert!(ok, "check --json failed: stderr={err}");

    // Then their stdout byte-matches the committed B9 baseline.
    assert_eq!(
        text,
        snapshot("check_text.stdout"),
        "check text stdout drifted from the committed baseline"
    );
    assert_eq!(
        json,
        snapshot("check_json.stdout"),
        "check --json stdout drifted from the committed baseline"
    );
}
