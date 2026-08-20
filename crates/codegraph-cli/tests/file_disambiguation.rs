//! `--file` disambiguation for `callers` / `callees` / `impact` (upstream #1512).
//!
//! Two same-named definitions merged into ONE unlabeled list before, so an agent
//! could not tell which definition a caller belonged to and had no way to ask
//! about just one. Each test drives the REAL binary over a temp project, because
//! the defect is in the CLI's symbol-selection surface, not in the graph.
//!
//! Every assertion names the FILE of the returned relatives: a test that only
//! counted results would pass with the filter selecting the wrong definition.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-file-filter-{label}-{}-{}",
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
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// A project with `target()` defined TWICE — `alpha.ts` and `beta.ts` — each
/// called by its own distinctly-named caller, so a returned caller's NAME
/// identifies which definition was selected.
fn two_definition_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("alpha.ts"),
        "export function target(): number {\n  return leafAlpha();\n}\n\
         export function leafAlpha(): number {\n  return 1;\n}\n\
         export function callerAlpha(): number {\n  return target();\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("beta.ts"),
        "export function target(): number {\n  return leafBeta();\n}\n\
         export function leafBeta(): number {\n  return 2;\n}\n\
         export function callerBeta(): number {\n  return target();\n}\n",
    )
    .unwrap();
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    project
}

fn json_of(args: &[&str]) -> serde_json::Value {
    let (stdout, err, ok) = cli(args);
    assert!(ok, "command failed: stdout={stdout} stderr={err}");
    serde_json::from_str(&stdout).expect("valid JSON on stdout")
}

fn names(value: &serde_json::Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = value[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` must be an array in {value}"))
        .iter()
        .map(|n| n["name"].as_str().unwrap_or_default().to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn callers_without_file_returns_both_definitions_callers() {
    let dir = TestDir::new("callers-all");
    let project = two_definition_project(&dir);
    let p = project.to_str().unwrap();
    let v = json_of(&["callers", "target", "-p", p, "--json"]);
    assert_eq!(
        names(&v, "callers"),
        vec!["callerAlpha".to_string(), "callerBeta".to_string()],
        "unfiltered behaviour must be unchanged: both definitions' callers"
    );
}

#[test]
fn callers_with_file_selects_only_that_definition() {
    let dir = TestDir::new("callers-filtered");
    let project = two_definition_project(&dir);
    let p = project.to_str().unwrap();
    let v = json_of(&["callers", "target", "-p", p, "--file", "alpha.ts", "--json"]);
    assert_eq!(
        names(&v, "callers"),
        vec!["callerAlpha".to_string()],
        "--file alpha.ts must return ONLY alpha.ts's caller"
    );
    assert_eq!(
        v["file"], "alpha.ts",
        "the applied filter must be echoed in the JSON"
    );
}

#[test]
fn callees_with_file_selects_only_that_definition() {
    let dir = TestDir::new("callees-filtered");
    let project = two_definition_project(&dir);
    let p = project.to_str().unwrap();
    let v = json_of(&["callees", "target", "-p", p, "--file", "beta.ts", "--json"]);
    assert_eq!(
        names(&v, "callees"),
        vec!["leafBeta".to_string()],
        "--file beta.ts must return ONLY beta.ts's callee"
    );
}

#[test]
fn impact_with_file_selects_only_that_definition() {
    let dir = TestDir::new("impact-filtered");
    let project = two_definition_project(&dir);
    let p = project.to_str().unwrap();
    let all = json_of(&["impact", "target", "-p", p, "--depth", "3", "--json"]);
    let filtered = json_of(&[
        "impact", "target", "-p", p, "--depth", "3", "--file", "alpha.ts", "--json",
    ]);
    let affected = names(&filtered, "affected");
    assert!(
        affected.contains(&"callerAlpha".to_string()),
        "alpha.ts's impact must include callerAlpha: {affected:?}"
    );
    assert!(
        !affected.contains(&"callerBeta".to_string()),
        "alpha.ts's impact must NOT include beta.ts's caller: {affected:?}"
    );
    assert!(
        filtered["nodeCount"].as_u64().unwrap() < all["nodeCount"].as_u64().unwrap(),
        "filtering must narrow the radius: filtered={} all={}",
        filtered["nodeCount"],
        all["nodeCount"]
    );
}

#[test]
fn file_filter_accepts_a_path_suffix() {
    let dir = TestDir::new("suffix");
    let project = dir.path().join("proj");
    fs::create_dir_all(project.join("src/deep")).unwrap();
    fs::write(
        project.join("src/deep/mod.ts"),
        "export function target(): number {\n  return 1;\n}\n\
         export function callerDeep(): number {\n  return target();\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/other.ts"),
        "export function target(): number {\n  return 2;\n}\n\
         export function callerOther(): number {\n  return target();\n}\n",
    )
    .unwrap();
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let v = json_of(&[
        "callers",
        "target",
        "-p",
        p,
        "--file",
        "deep/mod.ts",
        "--json",
    ]);
    assert_eq!(
        names(&v, "callers"),
        vec!["callerDeep".to_string()],
        "a trailing path segment must be enough to disambiguate"
    );
}

#[test]
fn file_filter_matching_nothing_is_an_explicit_error() {
    // Silently returning "no callers" would read as "this symbol is dead" —
    // the same false-negative the filter exists to prevent.
    let dir = TestDir::new("nomatch");
    let project = two_definition_project(&dir);
    let p = project.to_str().unwrap();
    let (stdout, stderr, ok) = cli(&[
        "callers",
        "target",
        "-p",
        p,
        "--file",
        "nosuch.ts",
        "--json",
    ]);
    assert!(
        !ok,
        "an unmatched --file must fail, not report an empty result: {stdout}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("nosuch.ts"),
        "the error must name the rejected filter: {combined}"
    );
    assert!(
        combined.contains("alpha.ts") && combined.contains("beta.ts"),
        "the error must list the files that DO define the symbol: {combined}"
    );
}

#[test]
fn file_filter_is_documented_in_help_for_all_three_commands() {
    for command in ["callers", "callees", "impact"] {
        let (stdout, _, ok) = cli(&[command, "--help"]);
        assert!(ok, "{command} --help failed");
        assert!(
            stdout.contains("--file"),
            "{command} --help must document --file:\n{stdout}"
        );
    }
}
