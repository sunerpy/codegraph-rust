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
fn audit_dangling_does_not_report_a_scene_connection_method() {
    // Given the fixture indexed (main.tscn wires a `pressed` signal to the
    // _on_Close_pressed handler that exists in the sibling player.gd),
    let (_dir, project) = indexed_project("signal-method");
    let p = project.to_str().unwrap();

    // When audit runs with --dangling --json,
    let (stdout, err, ok) = cli(&["audit", "--dangling", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("audit emits valid JSON");

    // Then the bare connection method is NOT listed as a dangling path.
    let dangling_targets: Vec<&str> = value["dangling"]
        .as_array()
        .expect("dangling array")
        .iter()
        .map(|d| d["targetPath"].as_str().expect("targetPath"))
        .collect();
    assert!(
        !dangling_targets.contains(&"_on_Close_pressed"),
        "signal handler _on_Close_pressed must not be dangling, got: {dangling_targets:?}"
    );
}

#[test]
fn audit_orphans_exclude_drops_matching_prefix() {
    // Given the fixture with an orphan resource planted under levels/ (an
    // indexed subdir; addons/ and vendor/ are index-ignored so they cannot
    // exercise the CLI-layer prefix filter),
    let (_dir, project) = indexed_project("exclude-prefix");
    let p = project.to_str().unwrap();
    let levels = project.join("levels");
    fs::create_dir_all(&levels).unwrap();
    fs::copy(
        project.join("orphan.tres"),
        levels.join("level_orphan.tres"),
    )
    .unwrap();
    let (_o, _e, ok) = cli(&["index", "--force", p]);
    assert!(ok, "reindex failed");

    // When audit --orphans runs without and with --exclude levels/,
    let unfiltered: serde_json::Value = {
        let (stdout, err, ok) = cli(&["audit", "--orphans", "--json", "-p", p]);
        assert!(ok, "audit failed: stderr={err}");
        serde_json::from_str(&stdout).expect("valid JSON")
    };
    let filtered: serde_json::Value = {
        let (stdout, err, ok) = cli(&[
            "audit",
            "--orphans",
            "--exclude",
            "levels/",
            "--json",
            "-p",
            p,
        ]);
        assert!(ok, "audit failed: stderr={err}");
        serde_json::from_str(&stdout).expect("valid JSON")
    };

    // Then the levels/ orphan is present unfiltered but dropped by --exclude.
    let orphans = |v: &serde_json::Value| -> Vec<String> {
        v["orphans"]
            .as_array()
            .expect("orphans array")
            .iter()
            .map(|o| o["filePath"].as_str().expect("filePath").to_string())
            .collect()
    };
    assert!(
        orphans(&unfiltered)
            .iter()
            .any(|f| f.starts_with("levels/")),
        "levels orphan must appear unfiltered, got: {:?}",
        orphans(&unfiltered)
    );
    assert!(
        !orphans(&filtered).iter().any(|f| f.starts_with("levels/")),
        "--exclude levels/ must drop levels orphans, got: {:?}",
        orphans(&filtered)
    );
}

#[test]
fn audit_dangling_include_keeps_only_matching_prefix() {
    // Given the fixture with a Data/ resource whose ref target is missing,
    let (_dir, project) = indexed_project("include-data");
    let p = project.to_str().unwrap();
    let data = project.join("Data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("config.tres"),
        "[gd_resource type=\"Resource\" format=3]\n\n[ext_resource type=\"Resource\" path=\"res://Data/gone.tres\" id=\"1\"]\n\n[resource]\nlinked = ExtResource(\"1\")\n",
    )
    .unwrap();
    let (_o, _e, ok) = cli(&["index", "--force", p]);
    assert!(ok, "reindex failed");

    // When audit --dangling runs with --include Data/,
    let (stdout, err, ok) = cli(&[
        "audit",
        "--dangling",
        "--include",
        "Data/",
        "--json",
        "-p",
        p,
    ]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then every reported dangling ref originates under Data/.
    let from_files: Vec<&str> = value["dangling"]
        .as_array()
        .expect("dangling array")
        .iter()
        .map(|d| d["fromFile"].as_str().expect("fromFile"))
        .collect();
    assert!(
        !from_files.is_empty() && from_files.iter().all(|f| f.starts_with("Data/")),
        "--include Data/ must keep only Data/ refs, got: {from_files:?}"
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
fn audit_impact_rows_carry_the_changed_path_as_target() {
    // Given the fixture indexed (data.tres references referenced.tres),
    let (_dir, project) = indexed_project("impact-target");
    let p = project.to_str().unwrap();

    // When audit --impact referenced.tres --json runs,
    let (stdout, err, ok) = cli(&["audit", "--impact", "referenced.tres", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then every affected row echoes target=referenced.tres and there is no note.
    let affected = value["impact"]["affected"]
        .as_array()
        .expect("affected array");
    assert!(!affected.is_empty(), "referenced.tres must have referrers");
    assert!(
        affected
            .iter()
            .all(|a| a["target"].as_str() == Some("referenced.tres")),
        "every affected row must carry target=referenced.tres, got: {affected:?}"
    );
    assert!(
        value.get("note").is_none(),
        "a non-empty impact must NOT emit a boundary note"
    );
}

#[test]
fn audit_impact_empty_on_godot_path_emits_boundary_note() {
    // Given the fixture indexed (orphan.tres is referenced by nothing),
    let (_dir, project) = indexed_project("impact-empty-note");
    let p = project.to_str().unwrap();

    // When audit --impact orphan.tres --json runs,
    let (stdout, err, ok) = cli(&["audit", "--impact", "orphan.tres", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then the impact is empty and a boundary note is emitted.
    assert!(
        value["impact"]["affected"]
            .as_array()
            .expect("affected array")
            .is_empty(),
        "orphan.tres must have no static referrers"
    );
    assert!(
        value["note"].as_str().is_some_and(|n| n.contains("godot")),
        "an empty godot impact must emit a boundary note, got: {value:?}"
    );
}

#[test]
fn audit_orphans_carry_reason_and_low_confidence_note_for_godot_resource() {
    // Given the fixture indexed (orphan.tres is an orphan godot resource),
    let (_dir, project) = indexed_project("orphan-confidence");
    let p = project.to_str().unwrap();

    // When audit --orphans --json runs,
    let (stdout, err, ok) = cli(&["audit", "--orphans", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then orphan.tres carries reason=no_path_reference, confidence=low, a note.
    let orphan = value["orphans"]
        .as_array()
        .expect("orphans array")
        .iter()
        .find(|o| o["filePath"].as_str() == Some("orphan.tres"))
        .expect("orphan.tres reported");
    assert_eq!(
        orphan["reason"].as_str(),
        Some("no_path_reference"),
        "orphan reason must be no_path_reference, got: {orphan:?}"
    );
    assert_eq!(
        orphan["confidence"].as_str(),
        Some("low"),
        "godot resource orphan must be low-confidence, got: {orphan:?}"
    );
    assert!(
        orphan["note"].as_str().is_some(),
        "low-confidence orphan must carry a note, got: {orphan:?}"
    );
}

#[test]
fn audit_verify_plan_groups_open_scenes_for_a_changed_script() {
    // Given the fixture indexed (main.tscn binds a script = res://player.gd),
    let (_dir, project) = indexed_project("verify-plan");
    let p = project.to_str().unwrap();

    // When audit --impact player.gd --verify-plan --json runs,
    let (stdout, err, ok) = cli(&[
        "audit",
        "--impact",
        "player.gd",
        "--verify-plan",
        "--json",
        "-p",
        p,
    ]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then the plan opens main.tscn and lists a reason for it.
    let plan = &value["verifyPlan"];
    assert_eq!(
        plan["changed"].as_str(),
        Some("player.gd"),
        "plan changed must echo the impact path, got: {plan:?}"
    );
    let open_scenes: Vec<&str> = plan["openScenes"]
        .as_array()
        .expect("openScenes array")
        .iter()
        .map(|s| s.as_str().expect("res path"))
        .collect();
    assert!(
        open_scenes.contains(&"res://main.tscn"),
        "main.tscn must be in openScenes, got: {open_scenes:?}"
    );
    assert!(
        plan["reasons"]
            .as_array()
            .expect("reasons array")
            .iter()
            .any(|r| r["file"].as_str() == Some("main.tscn")),
        "main.tscn must appear in reasons, got: {plan:?}"
    );
}

#[test]
fn audit_verify_plan_requires_impact() {
    // Given the indexed fixture,
    let (_dir, project) = indexed_project("verify-plan-requires-impact");
    let p = project.to_str().unwrap();
    // When audit --verify-plan runs without --impact,
    let (_out, _err, ok) = cli(&["audit", "--verify-plan", "--orphans", "-p", p]);
    // Then clap rejects it (verify-plan requires impact).
    assert!(!ok, "--verify-plan without --impact must fail");
}

#[test]
fn audit_impact_surfaces_edge_subkind_for_godot_ref() {
    // Given the fixture indexed (data.tres references referenced.tres via ExtResource),
    let (_dir, project) = indexed_project("impact-subkind");
    let p = project.to_str().unwrap();

    // When audit --impact referenced.tres --json runs,
    let (stdout, err, ok) = cli(&["audit", "--impact", "referenced.tres", "--json", "-p", p]);
    assert!(ok, "audit failed: stderr={err}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // Then the affected row from data.tres carries edgeSubkind=ext_resource.
    let row = value["impact"]["affected"]
        .as_array()
        .expect("affected array")
        .iter()
        .find(|a| a["fromFile"].as_str() == Some("data.tres"))
        .expect("data.tres referrer present");
    assert_eq!(
        row["edgeSubkind"].as_str(),
        Some("ext_resource"),
        "godot ExtResource ref must surface edgeSubkind, got: {row:?}"
    );
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
