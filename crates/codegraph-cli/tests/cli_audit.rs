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

// F3: `audit --impact`, `impact`, and `affected` must AGREE on the underlying
// impact set for a Godot script that a scene mounts via `script = ExtResource`.
// `main.tscn` has no tree-sitter grammar, so its `ext_resource path="res://player.gd"`
// ref lives ONLY in `unresolved_refs` (path-keyed) — the lane `audit` already reads
// and the lane `impact`/`affected` now UNION in. All three surface `main.tscn`.
#[test]
fn f3_impact_affected_audit_agree_on_godot_script_attach() {
    let dir = TestDir::new("f3-godot-agree");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let audit = run_in(
        dir.path(),
        &["audit", "-p", p, "--impact", "player.gd", "--json"],
    );
    assert!(audit.ok, "audit --impact must succeed: {}", audit.stderr);
    let audit_json: serde_json::Value =
        serde_json::from_str(&audit.stdout).expect("audit emits JSON");
    let audit_hits_scene = audit_json["impact"]["affected"]
        .as_array()
        .expect("impact.affected array")
        .iter()
        .any(|a| a["fromFile"].as_str() == Some("main.tscn"));
    assert!(
        audit_hits_scene,
        "audit --impact player.gd must list main.tscn: {}",
        audit.stdout
    );

    let impact = run_in(dir.path(), &["impact", "player.gd", "-p", p, "--json"]);
    assert!(impact.ok, "impact must succeed: {}", impact.stderr);
    let impact_json: serde_json::Value =
        serde_json::from_str(&impact.stdout).expect("impact emits JSON");
    let impact_hits_scene = impact_json["affected"]
        .as_array()
        .expect("affected array")
        .iter()
        .any(|a| a["filePath"].as_str() == Some("main.tscn"));
    assert!(
        impact_hits_scene,
        "impact player.gd must now surface main.tscn (path-keyed referrer): {}",
        impact.stdout
    );

    let affected = run_in(
        dir.path(),
        &["affected", "player.gd", "-p", p, "--depth", "5"],
    );
    assert!(affected.ok, "affected must succeed: {}", affected.stderr);
    let affected_json: serde_json::Value =
        serde_json::from_str(&affected.stdout).expect("affected emits JSON");
    let traversed = affected_json["totalDependentsTraversed"]
        .as_u64()
        .expect("totalDependentsTraversed");
    assert!(
        traversed >= 1,
        "affected player.gd --depth 5 must traverse the main.tscn referrer: {}",
        affected.stdout
    );
}

// F3 no-pollution guard: the path-keyed referrer fold-in fires ONLY when the
// impact target resolves to a script FILE node, never for a symbol INSIDE a
// script. Querying the `_on_Close_pressed` function (not the `player.gd` file)
// must return only its own symbol graph — no synthetic `.tscn`/File rows.
#[test]
fn f3_non_godot_impact_unaffected_by_path_keyed_lane() {
    let dir = TestDir::new("f3-non-godot");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let impact = run_in(
        dir.path(),
        &["impact", "_on_Close_pressed", "-p", p, "--json"],
    );
    assert!(impact.ok, "impact must succeed: {}", impact.stderr);
    let impact_json: serde_json::Value =
        serde_json::from_str(&impact.stdout).expect("impact emits JSON");
    let has_file_row = impact_json["affected"]
        .as_array()
        .expect("affected array")
        .iter()
        .any(|a| a["kind"].as_str() == Some("file"));
    assert!(
        !has_file_row,
        "a symbol-level impact target must not fold in path-keyed File referrers: {}",
        impact.stdout
    );
}
