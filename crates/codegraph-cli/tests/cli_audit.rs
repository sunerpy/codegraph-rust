//! CLI integration tests for the `audit` subcommand (orphans / dangling /
//! impact / verify-plan, text + JSON, include/exclude filters) driven against
//! the committed `tests/fixtures/godot_audit/` project in a private temp dir
//! with an isolated HTTP registry.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/godot_audit")
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

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn run_in(registry_dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(bin())
        .args(args)
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("godot_audit");
    copy_tree(&fixture(), &project);
    let p = project.to_str().unwrap();
    let init = run_in(dir.path(), &["init", p]);
    assert!(init.ok, "init failed: {} {}", init.stdout, init.stderr);
    let idx = run_in(dir.path(), &["index", "--force", p]);
    assert!(idx.ok, "index failed: {} {}", idx.stdout, idx.stderr);
    project
}

#[test]
fn audit_orphans_text_lists_unreferenced_resources() {
    let dir = TestDir::new("orphans-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["audit", "-p", p, "--orphans"]);
    assert!(run.ok, "audit --orphans must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("orphan resources") && run.stdout.contains("orphan.tres"),
        "orphans text must list orphan.tres: {}",
        run.stdout
    );
}

#[test]
fn audit_dangling_text_lists_missing_targets() {
    let dir = TestDir::new("dangling-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["audit", "-p", p, "--dangling"]);
    assert!(run.ok, "audit --dangling must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("dangling references") && run.stdout.contains("ghost.tres"),
        "dangling text must name the missing target: {}",
        run.stdout
    );
}

#[test]
fn audit_impact_text_reports_referencing_sites() {
    let dir = TestDir::new("impact-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["audit", "-p", p, "--impact", "referenced.tres"],
    );
    assert!(run.ok, "audit --impact must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("referenced.tres is referenced by") && run.stdout.contains("data.tres"),
        "impact text must list the referencing site: {}",
        run.stdout
    );
}

#[test]
fn audit_impact_empty_godot_path_prints_note() {
    let dir = TestDir::new("impact-empty");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["audit", "-p", p, "--impact", "orphan.tres"]);
    assert!(run.ok, "audit --impact orphan must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Nothing references orphan.tres")
            && run.stdout.contains("no static references found"),
        "empty godot impact must print the note: {}",
        run.stdout
    );
}

#[test]
fn audit_impact_verify_plan_text_categorizes_targets() {
    let dir = TestDir::new("verify-plan-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["audit", "-p", p, "--impact", "player.gd", "--verify-plan"],
    );
    assert!(run.ok, "audit --verify-plan must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("verify-plan for player.gd")
            && run.stdout.contains("res://player.gd")
            && run.stdout.contains("res://main.tscn"),
        "verify-plan text must categorize scripts and scenes: {}",
        run.stdout
    );
}

#[test]
fn audit_all_modes_json_carries_every_section() {
    let dir = TestDir::new("audit-json");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &[
            "audit",
            "-p",
            p,
            "--orphans",
            "--dangling",
            "--impact",
            "player.gd",
            "--verify-plan",
            "--json",
        ],
    );
    assert!(run.ok, "audit --json must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("audit emits JSON");
    assert!(value["orphans"].is_array(), "json must carry orphans");
    assert!(value["dangling"].is_array(), "json must carry dangling");
    assert_eq!(value["impact"]["changed"], serde_json::json!("player.gd"));
    assert!(
        value["verifyPlan"]["loadScripts"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "verifyPlan must list load scripts: {}",
        run.stdout
    );
}

#[test]
fn audit_impact_empty_json_carries_note() {
    let dir = TestDir::new("impact-json-note");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["audit", "-p", p, "--impact", "orphan.tres", "--json"],
    );
    assert!(run.ok, "audit --impact --json must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("audit JSON");
    assert!(
        value["note"]
            .as_str()
            .is_some_and(|n| n.contains("no static references found")),
        "empty impact json must carry the note: {}",
        run.stdout
    );
}

#[test]
fn audit_include_filter_drops_non_matching_prefixes() {
    let dir = TestDir::new("include-filter");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["audit", "-p", p, "--orphans", "--include", "src/"],
    );
    assert!(run.ok, "audit --include must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("No orphan resources found"),
        "an include prefix no file matches must empty the list: {}",
        run.stdout
    );
}

#[test]
fn audit_exclude_filter_drops_matching_prefixes() {
    let dir = TestDir::new("exclude-filter");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["audit", "-p", p, "--orphans", "--exclude", "data"],
    );
    assert!(run.ok, "audit --exclude must succeed: {}", run.stderr);
    assert!(
        !run.stdout.contains("data.tres") && run.stdout.contains("orphan.tres"),
        "exclude prefix must drop data.tres but keep orphan.tres: {}",
        run.stdout
    );
}
