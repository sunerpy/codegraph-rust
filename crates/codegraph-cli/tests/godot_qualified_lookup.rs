//! CLI integration test for GDScript qualified-name (`Class.member`) query
//! equivalence (R1 of godot-static-graph).
//!
//! Drives the real `codegraph` binary against the committed
//! `tests/fixtures/godot_qualified/` project. A GDScript `class_name
//! DamageCalculator` global stores its methods as top-level `Function` nodes
//! named by the SHORT name only (the `class_name` global is not pushed on the
//! node stack), so there is no dotted `DamageCalculator.calc_skill_damage` node
//! anywhere in the graph. These tests assert that the CLI lookup resolves the
//! dotted receiver form to the same node the short name resolves to, for
//! `callers`, `impact`, and `query` — plus the negative and regression guards.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/godot_qualified")
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
            "codegraph-cli-godot-qualified-{label}-{}-{}",
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
    let project = dir.path().join("godot_qualified");
    copy_tree(&fixture(), &project);
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");
    (dir, project)
}

/// The set of caller names in a `callers ... --json` payload, sorted.
fn caller_names(json: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = json["callers"]
        .as_array()
        .expect("callers array")
        .iter()
        .map(|c| c["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    names
}

/// The set of affected node names in an `impact ... --json` payload, sorted.
fn affected_names(json: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = json["affected"]
        .as_array()
        .expect("affected array")
        .iter()
        .map(|c| c["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    names
}

fn run_json(args: &[&str]) -> serde_json::Value {
    let (stdout, err, ok) = cli(args);
    assert!(ok, "command {args:?} failed: stderr={err}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid JSON from {args:?}: {e}\n{stdout}"))
}

#[test]
fn callers_dotted_matches_short_name() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("callers");
    let p = project.to_str().unwrap();

    // When callers runs for both the short and the dotted receiver form,
    let short = run_json(&["callers", "calc_skill_damage", "-p", p, "--json"]);
    let dotted = run_json(&[
        "callers",
        "DamageCalculator.calc_skill_damage",
        "-p",
        p,
        "--json",
    ]);

    // Then the short name resolves to the calling function,
    let short_callers = caller_names(&short);
    assert_eq!(
        short_callers,
        vec!["process_skill_hit".to_string()],
        "short-name callers must be the calling function"
    );
    // And the dotted form resolves to the SAME caller(s).
    assert_eq!(
        caller_names(&dotted),
        short_callers,
        "dotted-form callers must equal short-name callers"
    );
}

#[test]
fn impact_dotted_matches_short_name() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("impact");
    let p = project.to_str().unwrap();

    // When impact runs for both the short and the dotted receiver form,
    let short = run_json(&["impact", "calc_skill_damage", "-p", p, "--json"]);
    let dotted = run_json(&[
        "impact",
        "DamageCalculator.calc_skill_damage",
        "-p",
        p,
        "--json",
    ]);

    // Then the dotted form's affected set equals the short name's (non-empty).
    let short_affected = affected_names(&short);
    assert!(
        !short_affected.is_empty(),
        "short-name impact must be non-empty"
    );
    assert_eq!(
        affected_names(&dotted),
        short_affected,
        "dotted-form impact must equal short-name impact"
    );
}

#[test]
fn query_dotted_returns_the_member_function_node() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("query");
    let p = project.to_str().unwrap();

    // When query runs for the dotted receiver form,
    let dotted = run_json(&[
        "query",
        "DamageCalculator.calc_skill_damage",
        "-p",
        p,
        "--json",
    ]);

    // Then it returns the `calc_skill_damage` Function node (non-empty).
    let arr = dotted.as_array().expect("query returns an array");
    assert!(
        !arr.is_empty(),
        "query for the dotted form must be non-empty"
    );
    assert!(
        arr.iter().any(|r| {
            r["node"]["name"] == "calc_skill_damage"
                && r["node"]["kind"] == "function"
                && r["node"]["filePath"] == "damage_calculator.gd"
        }),
        "query must return the calc_skill_damage Function node in damage_calculator.gd, got: {dotted}"
    );
}

#[test]
fn query_dotted_resolves_second_member() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("query2");
    let p = project.to_str().unwrap();

    // When query runs for the other dotted member,
    let dotted = run_json(&[
        "query",
        "DamageCalculator.calc_pf_damage",
        "-p",
        p,
        "--json",
    ]);

    // Then it resolves to the calc_pf_damage Function node.
    let arr = dotted.as_array().expect("query returns an array");
    assert!(
        arr.iter().any(|r| {
            r["node"]["name"] == "calc_pf_damage"
                && r["node"]["kind"] == "function"
                && r["node"]["filePath"] == "damage_calculator.gd"
        }),
        "query must resolve DamageCalculator.calc_pf_damage, got: {dotted}"
    );
}

#[test]
fn dotted_nonexistent_member_is_empty() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("neg-member");
    let p = project.to_str().unwrap();

    // When callers runs for a real class with a nonexistent member,
    let dotted = run_json(&["callers", "DamageCalculator.nonexistent", "-p", p, "--json"]);

    // Then no caller is fabricated.
    assert!(
        caller_names(&dotted).is_empty(),
        "a nonexistent member must not resolve to any caller"
    );
}

#[test]
fn dotted_non_class_receiver_falls_through() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("neg-recv");
    let p = project.to_str().unwrap();

    // When callers runs for a dotted symbol whose receiver is NOT a class node,
    let (stdout, err, ok) = cli(&["callers", "notaclass.calc_skill_damage", "-p", p, "--json"]);

    // Then the command still succeeds (falls through to normal search, no crash)
    assert!(ok, "non-class receiver lookup must not crash: stderr={err}");
    let dotted: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // And it does NOT spuriously resolve the class member.
    assert!(
        caller_names(&dotted).is_empty(),
        "a non-class receiver must not trigger class-member resolution, got: {dotted}"
    );
}

#[test]
fn plain_short_name_callers_still_works() {
    // Given the indexed godot_qualified fixture,
    let (_dir, project) = indexed_project("regression");
    let p = project.to_str().unwrap();

    // When callers runs for the plain short name,
    let short = run_json(&["callers", "calc_skill_damage", "-p", p, "--json"]);

    // Then it resolves to the calling function exactly as before.
    assert_eq!(
        caller_names(&short),
        vec!["process_skill_hit".to_string()],
        "plain short-name callers must be unchanged"
    );
}
