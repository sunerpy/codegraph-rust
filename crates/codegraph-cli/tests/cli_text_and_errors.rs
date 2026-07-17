//! CLI integration tests targeting the TEXT-output (non-`--json`) branches and
//! error paths of the traversal/query commands that the JSON-focused suites
//! leave uncovered. Drives the real `codegraph` binary against the committed
//! mini fixture (`crates/codegraph-bench/fixtures/mini`) in a private temp
//! project with an isolated HTTP registry.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-text-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
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
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();
    let init = run_in(dir.path(), &["init", p]);
    assert!(init.ok, "init failed: {} {}", init.stdout, init.stderr);
    let idx = run_in(dir.path(), &["index", "--force", p]);
    assert!(idx.ok, "index failed: {} {}", idx.stdout, idx.stderr);
    project
}

#[test]
fn query_text_with_results_prints_header_and_signature() {
    let dir = TestDir::new("query-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["query", "add", "-p", p]);
    assert!(run.ok, "query text must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Search Results for \"add\""),
        "text query must print the results header: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("add"),
        "text query must list the matched symbol: {}",
        run.stdout
    );
}

#[test]
fn query_text_with_kind_filter_and_limit() {
    let dir = TestDir::new("query-kind");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &[
            "query", "Counter", "-p", p, "--kind", "class", "--limit", "5",
        ],
    );
    assert!(run.ok, "query --kind must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Counter") || run.stdout.contains("No results found"),
        "kind-filtered query must render deterministically: {}",
        run.stdout
    );
}

#[test]
fn query_rejects_unknown_kind() {
    let dir = TestDir::new("query-badkind");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["query", "add", "-p", p, "--kind", "not-a-kind"],
    );
    assert!(!run.ok, "an unknown --kind must exit non-zero");
}

#[test]
fn callers_callees_text_output_render() {
    let dir = TestDir::new("related-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let callers = run_in(dir.path(), &["callers", "add", "-p", p]);
    assert!(callers.ok, "callers text must succeed: {}", callers.stderr);
    assert!(
        callers.stdout.contains("Callers"),
        "callers text must carry the Callers banner: {}",
        callers.stdout
    );

    let callees = run_in(dir.path(), &["callees", "runDemo", "-p", p]);
    assert!(callees.ok, "callees text must succeed: {}", callees.stderr);
    assert!(
        callees.stdout.contains("Callees"),
        "callees text must carry the Callees banner: {}",
        callees.stdout
    );
}

#[test]
fn callers_of_unknown_symbol_render_empty_text() {
    let dir = TestDir::new("callers-none");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["callers", "no_such_symbol_zzz", "-p", p]);
    assert!(
        run.ok,
        "callers of missing symbol must succeed: {}",
        run.stderr
    );
    assert!(
        run.stdout.contains("No callers found for"),
        "empty callers prints the no-callers line: {}",
        run.stdout
    );
}

#[test]
fn impact_text_output_and_not_found_branch() {
    let dir = TestDir::new("impact-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let hit = run_in(dir.path(), &["impact", "add", "-p", p]);
    assert!(hit.ok, "impact text must succeed: {}", hit.stderr);
    assert!(
        hit.stdout.contains("Impact of changing \"add\""),
        "impact text must print the impact header: {}",
        hit.stdout
    );

    let miss = run_in(dir.path(), &["impact", "no_such_symbol_zzz", "-p", p]);
    assert!(
        miss.ok,
        "impact of missing symbol must succeed: {}",
        miss.stderr
    );
    assert!(
        miss.stdout.contains("not found"),
        "missing symbol must print the not-found line: {}",
        miss.stdout
    );
}

#[test]
fn check_text_output_reports_no_cycles() {
    let dir = TestDir::new("check-text");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["check", "-p", p]);
    assert!(run.ok, "check text must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("No circular dependencies found"),
        "the tiny fixture must report no cycles in text mode: {}",
        run.stdout
    );
}

#[test]
fn files_text_default_tree_render() {
    let dir = TestDir::new("files-tree");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["files", "-p", p]);
    assert!(run.ok, "files tree must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains(".ts") || run.stdout.contains(".py"),
        "files tree must list source files: {}",
        run.stdout
    );
}

#[test]
fn files_empty_criteria_reports_no_matches() {
    let dir = TestDir::new("files-empty");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["files", "-p", p, "--language", "nosuchlang"]);
    assert!(run.ok, "files with no matches must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("No files found matching the criteria."),
        "empty text result must print the no-matches line: {}",
        run.stdout
    );
}

#[test]
fn affected_json_lists_tests_for_a_changed_file() {
    let dir = TestDir::new("affected-json");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["affected", "-p", p, "src/math.ts"]);
    assert!(run.ok, "affected must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("affected emits JSON");
    assert!(
        value.get("affectedTests").is_some(),
        "affected JSON must carry affectedTests: {}",
        run.stdout
    );
}

#[test]
fn affected_traverses_dependents_at_depth() {
    let dir = TestDir::new("affected-depth");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["affected", "-p", p, "--depth", "3", "src/math.ts"],
    );
    assert!(run.ok, "affected --depth must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("affected JSON");
    assert!(
        value["totalDependentsTraversed"].as_i64().unwrap() >= 1,
        "changing src/math.ts must traverse its dependent app.ts: {}",
        run.stdout
    );
}

#[test]
fn affected_with_filter_treats_matches_as_tests() {
    let dir = TestDir::new("affected-filter");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["affected", "-p", p, "--filter", "src/*", "src/math.ts"],
    );
    assert!(run.ok, "affected --filter must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("affected JSON");
    let tests: Vec<String> = value["affectedTests"]
        .as_array()
        .expect("affectedTests array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        tests.contains(&"src/math.ts".to_string()),
        "a --filter match must count the changed file as a test: {}",
        run.stdout
    );
}

#[test]
fn node_missing_is_zero_but_strict_is_nonzero() {
    let dir = TestDir::new("strict-node");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let lax = run_in(dir.path(), &["node", "no_such_symbol_zzz", "-p", p]);
    assert!(lax.ok, "node of a missing symbol must exit 0 by default");

    let strict = run_in(
        dir.path(),
        &["node", "no_such_symbol_zzz", "-p", p, "--strict"],
    );
    assert!(
        !strict.ok,
        "node --strict of a missing symbol must exit non-zero: {}",
        strict.stdout
    );

    let hit = run_in(dir.path(), &["node", "add", "-p", p, "--strict"]);
    assert!(
        hit.ok,
        "node --strict of an existing symbol must exit 0: {}",
        hit.stderr
    );
}

#[test]
fn query_and_impact_strict_flags_gate_exit_code() {
    let dir = TestDir::new("strict-query-impact");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let q_lax = run_in(dir.path(), &["query", "no_such_symbol_zzz", "-p", p]);
    assert!(q_lax.ok, "query of a missing symbol must exit 0 by default");
    let q_strict = run_in(
        dir.path(),
        &["query", "no_such_symbol_zzz", "-p", p, "--strict"],
    );
    assert!(
        !q_strict.ok,
        "query --strict must exit non-zero on no results"
    );

    let i_lax = run_in(dir.path(), &["impact", "no_such_symbol_zzz", "-p", p]);
    assert!(
        i_lax.ok,
        "impact of a missing symbol must exit 0 by default"
    );
    let i_strict = run_in(
        dir.path(),
        &["impact", "no_such_symbol_zzz", "-p", p, "--strict"],
    );
    assert!(
        !i_strict.ok,
        "impact --strict must exit non-zero when not found"
    );
}

#[test]
fn node_strict_found_symbol_with_sentinel_body_exits_zero() {
    let dir = TestDir::new("strict-sentinel-body");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    std::fs::write(
        project.join("sentinel.ts"),
        "export function sentinelCarrier(): string {\n  return \"No results found and not found in the codebase\";\n}\n",
    )
    .unwrap();
    let p = project.to_str().unwrap();
    let init = run_in(dir.path(), &["init", p]);
    assert!(init.ok, "init failed: {} {}", init.stdout, init.stderr);
    let idx = run_in(dir.path(), &["index", "--force", p]);
    assert!(idx.ok, "index failed: {} {}", idx.stdout, idx.stderr);

    let strict = run_in(
        dir.path(),
        &["node", "sentinelCarrier", "-p", p, "--strict"],
    );
    assert!(
        strict.ok,
        "node --strict of a FOUND symbol must exit 0 even when its source body contains a not-found sentinel phrase: {}",
        strict.stdout
    );
}

#[test]
fn node_strict_missing_file_exits_nonzero() {
    let dir = TestDir::new("strict-missing-file");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(
        dir.path(),
        &["node", "some/missing_file.rs", "-p", p, "--strict"],
    );
    assert!(
        !run.ok,
        "node --strict of a missing file must exit non-zero (the missing-file sentinel is now flagged): {}",
        run.stdout
    );
}

#[test]
fn affected_help_documents_the_filter_glob() {
    let dir = TestDir::new("affected-help");
    let run = run_in(dir.path(), &["affected", "--help"]);
    assert!(run.ok, "affected --help must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("GLOB"),
        "affected --help must show the GLOB value name: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("affectedTests")
            && run.stdout.contains("does NOT filter affectedFiles"),
        "affected --help must document that --filter classifies affectedTests only: {}",
        run.stdout
    );
}

#[test]
fn index_without_init_errors() {
    let dir = TestDir::new("index-noinit");
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let run = run_in(dir.path(), &["index", bare.to_str().unwrap()]);
    assert!(!run.ok, "index on an uninitialized root must exit non-zero");
}

#[test]
fn sync_without_init_errors() {
    let dir = TestDir::new("sync-noinit");
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let run = run_in(dir.path(), &["sync", bare.to_str().unwrap()]);
    assert!(!run.ok, "sync on an uninitialized root must exit non-zero");
}

#[test]
fn callees_without_init_errors() {
    let dir = TestDir::new("callees-noinit");
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let run = run_in(
        dir.path(),
        &["callees", "foo", "-p", bare.to_str().unwrap()],
    );
    assert!(!run.ok, "callees without an index must exit non-zero");
    assert!(
        run.stderr.contains("not initialized"),
        "error must name the missing init: {}",
        run.stderr
    );
}

#[test]
fn export_pretty_and_centrality_to_stdout() {
    let dir = TestDir::new("export-centrality");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["export", "-p", p]);
    assert!(
        run.ok,
        "export with centrality must succeed: {}",
        run.stderr
    );
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("export emits JSON");
    assert!(
        value.get("nodes").is_some(),
        "export graph must carry nodes: {}",
        run.stdout
    );
}

#[test]
fn files_pattern_filter_keeps_only_glob_matches() {
    let dir = TestDir::new("files-pattern");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(
        dir.path(),
        &["files", "-p", p, "--pattern", "*.ts", "--json"],
    );
    assert!(run.ok, "files --pattern must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("files JSON");
    let paths: Vec<String> = value
        .as_array()
        .expect("array")
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !paths.is_empty() && paths.iter().all(|path| path.ends_with(".ts")),
        "pattern *.ts must keep only .ts files: {paths:?}"
    );
}

#[test]
fn files_flat_and_grouped_text_render() {
    let dir = TestDir::new("files-formats");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    for fmt in ["flat", "grouped"] {
        let run = run_in(dir.path(), &["files", "-p", p, "--format", fmt]);
        assert!(run.ok, "files --format {fmt} must succeed: {}", run.stderr);
        assert!(
            run.stdout.contains(".ts") || run.stdout.contains(".py"),
            "files --format {fmt} must list source files: {}",
            run.stdout
        );
    }
}

#[test]
fn status_json_initialized_carries_index_metadata() {
    let dir = TestDir::new("status-json-full");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["status", p, "--json"]);
    assert!(run.ok, "status --json must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("status JSON");
    assert_eq!(value["initialized"], serde_json::json!(true));
    assert_eq!(value["journalMode"], serde_json::json!("wal"));
    assert!(
        value["nodesByKind"].is_object(),
        "nodesByKind must be present"
    );
    assert!(
        value["index"]["currentExtractionVersion"]
            .as_i64()
            .is_some(),
        "index metadata must carry the extraction version: {}",
        run.stdout
    );
}

#[test]
fn sync_after_index_reports_a_summary() {
    let dir = TestDir::new("sync-summary");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["sync", p]);
    assert!(run.ok, "sync must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Synced:") && run.stdout.contains("skipped"),
        "sync must print the reindexed/skipped/removed summary: {}",
        run.stdout
    );
}

#[test]
fn sync_quiet_prints_no_summary() {
    let dir = TestDir::new("sync-quiet");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["sync", p, "--quiet"]);
    assert!(run.ok, "sync --quiet must succeed: {}", run.stderr);
    assert!(
        !run.stdout.contains("Synced:"),
        "sync --quiet must stay silent on stdout: {}",
        run.stdout
    );
}

#[test]
fn index_quiet_suppresses_the_result_banner() {
    let dir = TestDir::new("index-quiet");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["index", "--force", "--quiet", p]);
    assert!(run.ok, "index --quiet must succeed: {}", run.stderr);
    assert!(
        run.stdout.trim().is_empty(),
        "index --quiet must not print the result banner: {}",
        run.stdout
    );
}

#[test]
fn index_verbose_prints_the_result_banner() {
    let dir = TestDir::new("index-verbose");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["index", "--force", "--verbose", p]);
    assert!(run.ok, "index --verbose must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Indexed") && run.stdout.contains("nodes"),
        "index --verbose must print the result banner: {}",
        run.stdout
    );
}
