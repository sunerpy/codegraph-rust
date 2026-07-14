//! CLI integration tests for command dispatch, error paths, and the `http`
//! subcommand group against an isolated registry.
//!
//! Drives the real `codegraph` binary against the committed mini fixture
//! (`crates/codegraph-bench/fixtures/mini`). Every test uses a private temp
//! project and an isolated `CODEGRAPH_HTTP_REGISTRY_DIR` so HTTP-registry state
//! never leaks between tests or onto the developer machine.

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
            "codegraph-cli-cmd-{label}-{}-{}",
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

/// Run the binary with a private HTTP-registry dir so `http` subcommands never
/// touch the shared machine state.
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
    let run = run_in(dir.path(), &["init", p]);
    assert!(run.ok, "init failed: {} {}", run.stdout, run.stderr);
    project
}

#[test]
fn version_prints_semver_line() {
    let dir = TestDir::new("version");
    let run = run_in(dir.path(), &["version"]);
    assert!(run.ok, "version must succeed: {}", run.stderr);
    assert!(
        run.stdout.starts_with("codegraph "),
        "version output must start with `codegraph `: {}",
        run.stdout
    );
}

#[test]
fn status_on_uninitialized_reports_not_initialized() {
    let dir = TestDir::new("status-uninit");
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let run = run_in(dir.path(), &["status", empty.to_str().unwrap()]);
    assert!(
        run.ok,
        "status on uninitialized must still succeed: {}",
        run.stderr
    );
    assert!(
        run.stdout.contains("Not initialized"),
        "expected `Not initialized`: {}",
        run.stdout
    );
}

#[test]
fn status_json_uninitialized_has_initialized_false() {
    let dir = TestDir::new("status-json");
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let run = run_in(dir.path(), &["status", empty.to_str().unwrap(), "--json"]);
    assert!(run.ok, "status --json must succeed: {}", run.stderr);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("valid JSON");
    assert_eq!(value["initialized"], serde_json::json!(false));
    assert_eq!(value["dbExists"], serde_json::json!(false));
}

#[test]
fn query_without_index_errors_not_initialized() {
    let dir = TestDir::new("query-noindex");
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let run = run_in(dir.path(), &["query", "foo", "-p", empty.to_str().unwrap()]);
    assert!(!run.ok, "query without an index must exit non-zero");
    assert!(
        run.stderr.contains("not initialized"),
        "error must name the missing init: {}",
        run.stderr
    );
}

#[test]
fn index_then_query_status_check_roundtrip() {
    let dir = TestDir::new("roundtrip");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let idx = run_in(dir.path(), &["index", "--force", p]);
    assert!(idx.ok, "index --force must succeed: {}", idx.stderr);

    // query (json) returns a JSON array.
    let q = run_in(dir.path(), &["query", "Greeter", "-p", p, "--json"]);
    assert!(q.ok, "query must succeed: {}", q.stderr);
    let arr: serde_json::Value = serde_json::from_str(&q.stdout).expect("query emits JSON");
    assert!(
        arr.is_array(),
        "query --json must be an array: {}",
        q.stdout
    );

    // query (text) for a non-existent symbol prints the no-results line.
    let none = run_in(dir.path(), &["query", "no_such_symbol_zzz", "-p", p]);
    assert!(none.ok);
    assert!(
        none.stdout.contains("No results found"),
        "expected no-results text: {}",
        none.stdout
    );

    // status (text) shows the initialized banner.
    let st = run_in(dir.path(), &["status", p]);
    assert!(st.ok, "status must succeed: {}", st.stderr);
    assert!(
        st.stdout.contains("CodeGraph Status") && st.stdout.contains("Index Statistics"),
        "status banner missing: {}",
        st.stdout
    );

    // status (json) reports initialized true with counts.
    let stj = run_in(dir.path(), &["status", p, "--json"]);
    assert!(stj.ok);
    let sv: serde_json::Value = serde_json::from_str(&stj.stdout).expect("status JSON");
    assert_eq!(sv["initialized"], serde_json::json!(true));
    assert!(sv["fileCount"].as_i64().unwrap() >= 1);

    // check runs (no circular deps expected in the tiny fixture).
    let ck = run_in(dir.path(), &["check", "-p", p, "--json"]);
    assert!(ck.ok, "check must succeed: {}", ck.stderr);
    let cv: serde_json::Value = serde_json::from_str(&ck.stdout).expect("check JSON");
    assert!(cv["cycles"].is_array());

    // callers/callees/impact JSON surfaces resolve without error.
    let callers = run_in(dir.path(), &["callers", "Greeter", "-p", p, "--json"]);
    assert!(callers.ok, "callers must succeed: {}", callers.stderr);
    let callees = run_in(dir.path(), &["callees", "Greeter", "-p", p, "--json"]);
    assert!(callees.ok, "callees must succeed: {}", callees.stderr);
    let impact = run_in(dir.path(), &["impact", "Greeter", "-p", p, "--json"]);
    assert!(impact.ok, "impact must succeed: {}", impact.stderr);

    // files (all formats) succeed.
    for fmt in ["tree", "flat", "grouped"] {
        let f = run_in(dir.path(), &["files", "-p", p, "--format", fmt]);
        assert!(f.ok, "files --format {fmt} must succeed: {}", f.stderr);
    }
}

#[test]
fn affected_with_no_files_prints_hint() {
    let dir = TestDir::new("affected-empty");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["affected", "-p", p]);
    assert!(
        run.ok,
        "affected with no files must succeed: {}",
        run.stderr
    );
    assert!(
        run.stdout.contains("No files provided"),
        "expected the no-files hint: {}",
        run.stdout
    );
}

#[test]
fn audit_requires_a_mode_flag() {
    let dir = TestDir::new("audit-nomode");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["audit", "-p", p]);
    assert!(!run.ok, "audit with no mode flag must exit non-zero");
    assert!(
        run.stderr.contains("--orphans") || run.stderr.contains("--dangling"),
        "error must list the required mode flags: {}",
        run.stderr
    );
}

#[test]
fn export_to_stdout_and_file() {
    let dir = TestDir::new("export");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    // To stdout: valid JSON graph.
    let out = run_in(dir.path(), &["export", "-p", p, "--no-centrality"]);
    assert!(out.ok, "export must succeed: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("export JSON");
    assert!(v.get("nodes").is_some(), "export graph must have nodes");

    // To a file: writes and reports on stderr.
    let target = dir.path().join("graph.json");
    let outf = run_in(
        dir.path(),
        &["export", "-p", p, "-o", target.to_str().unwrap()],
    );
    assert!(outf.ok, "export -o must succeed: {}", outf.stderr);
    assert!(target.is_file(), "export must write the file");
    assert!(
        outf.stderr.contains("Exported"),
        "export -o must report on stderr: {}",
        outf.stderr
    );
}

#[test]
fn unlock_on_clean_project_reports_nothing_to_do() {
    let dir = TestDir::new("unlock-clean");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["unlock", p]);
    assert!(run.ok, "unlock must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("No lock file found") || run.stdout.contains("Removed lock file"),
        "unlock must report a clear outcome: {}",
        run.stdout
    );
}

#[test]
fn unlock_removes_existing_lock_file() {
    let dir = TestDir::new("unlock-lock");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let lock = project.join(".codegraph/codegraph.lock");
    std::fs::write(&lock, b"stale").unwrap();
    let run = run_in(dir.path(), &["unlock", p]);
    assert!(run.ok, "unlock must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Removed lock file"),
        "unlock must report removal: {}",
        run.stdout
    );
    assert!(!lock.exists(), "lock file must be gone after unlock");
}

#[test]
fn uninit_requires_force_then_removes() {
    let dir = TestDir::new("uninit");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    // Without --force: refuses.
    let refused = run_in(dir.path(), &["uninit", p]);
    assert!(!refused.ok, "uninit without --force must exit non-zero");
    assert!(
        refused.stderr.contains("--force"),
        "refusal must mention --force: {}",
        refused.stderr
    );
    assert!(
        project.join(".codegraph").exists(),
        "uninit without --force must not delete the index"
    );

    // With --force: removes .codegraph.
    let done = run_in(dir.path(), &["uninit", "--force", p]);
    assert!(done.ok, "uninit --force must succeed: {}", done.stderr);
    assert!(
        !project.join(".codegraph").exists(),
        "uninit --force must remove .codegraph"
    );
}

#[test]
fn serve_detach_without_http_errors() {
    let dir = TestDir::new("serve-detach");
    let run = run_in(dir.path(), &["serve", "--detach"]);
    assert!(!run.ok, "serve --detach without --http must exit non-zero");
    assert!(
        run.stderr.contains("--detach") && run.stderr.contains("--http"),
        "error must explain --detach needs --http: {}",
        run.stderr
    );
}

#[test]
fn http_list_on_empty_registry_reports_none() {
    let dir = TestDir::new("http-list");
    let run = run_in(dir.path(), &["http", "list"]);
    assert!(run.ok, "http list must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("No HTTP MCP servers running"),
        "empty registry must report none: {}",
        run.stdout
    );
}

#[test]
fn http_status_all_and_by_addr_on_empty_registry() {
    let dir = TestDir::new("http-status");
    // Status with no addr → the none/list line.
    let all = run_in(dir.path(), &["http", "status"]);
    assert!(all.ok, "http status must succeed: {}", all.stderr);
    assert!(
        all.stdout.contains("No HTTP MCP servers running"),
        "empty status (all) must report none: {}",
        all.stdout
    );
    // Status by a specific addr not running → the per-addr none line.
    let one = run_in(dir.path(), &["http", "status", "127.0.0.1:65535"]);
    assert!(one.ok, "http status <addr> must succeed: {}", one.stderr);
    assert!(
        one.stdout
            .contains("No HTTP MCP server running on 127.0.0.1:65535"),
        "per-addr status must name the addr: {}",
        one.stdout
    );
}

#[test]
fn http_stop_on_absent_addr_reports_none() {
    let dir = TestDir::new("http-stop");
    let run = run_in(dir.path(), &["http", "stop", "127.0.0.1:65534"]);
    assert!(
        run.ok,
        "http stop on an absent addr must succeed: {}",
        run.stderr
    );
    assert!(
        run.stdout
            .contains("No HTTP MCP server running on 127.0.0.1:65534"),
        "http stop must report the absent addr: {}",
        run.stdout
    );
}

#[test]
fn init_on_already_initialized_is_idempotent() {
    let dir = TestDir::new("init-again");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["init", p]);
    assert!(run.ok, "re-init must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Already initialized"),
        "re-init must report already-initialized: {}",
        run.stdout
    );
}

#[test]
fn prompt_hook_without_index_is_silent() {
    // Under the confidence-tiered gate, the hook is degradable-by-contract: an
    // unindexed dir (or a non-matching prompt) exits 0 with NO output — the
    // upstream silent no-op, not the old "[codegraph] no index" notice.
    let dir = TestDir::new("prompt-hook");
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let run = run_in(
        dir.path(),
        &[
            "prompt-hook",
            "-p",
            empty.to_str().unwrap(),
            "how does this work",
        ],
    );
    assert!(run.ok, "prompt-hook must always succeed: {}", run.stderr);
    assert!(
        run.stdout.trim().is_empty(),
        "prompt-hook in an unindexed dir must be a silent no-op: {}",
        run.stdout
    );
}

#[test]
fn prompt_hook_empty_query_is_silent() {
    let dir = TestDir::new("prompt-hook-empty");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();
    let run = run_in(dir.path(), &["prompt-hook", "-p", p, "   "]);
    assert!(run.ok, "prompt-hook must succeed: {}", run.stderr);
    assert!(
        run.stdout.trim().is_empty(),
        "blank query must be a silent no-op: {}",
        run.stdout
    );
}
