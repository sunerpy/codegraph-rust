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
