//! CLI integration tests for the `explore` and `node` subcommands, which reuse
//! the MCP `codegraph_explore` / `codegraph_node` engine.
//!
//! Drives the real `codegraph` binary against the committed mini fixture
//! (`crates/codegraph-bench/fixtures/mini`) in a private temp project.

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
            "codegraph-cli-en-{label}-{}-{}",
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

fn run_in(cwd: &Path, args: &[&str]) -> Run {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
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
fn help_lists_explore_and_node_subcommands() {
    let dir = TestDir::new("help");
    let run = run_in(dir.path(), &["--help"]);
    assert!(run.ok, "--help must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("explore"),
        "top-level --help must list `explore`: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("node"),
        "top-level --help must list `node`: {}",
        run.stdout
    );

    // Neither subcommand may report `unrecognized subcommand`.
    let eh = run_in(dir.path(), &["explore", "--help"]);
    assert!(eh.ok, "explore --help must succeed: {}", eh.stderr);
    assert!(
        !eh.stderr.contains("unrecognized subcommand"),
        "explore must be a real subcommand: {}",
        eh.stderr
    );
    let nh = run_in(dir.path(), &["node", "--help"]);
    assert!(nh.ok, "node --help must succeed: {}", nh.stderr);
    assert!(
        !nh.stderr.contains("unrecognized subcommand"),
        "node must be a real subcommand: {}",
        nh.stderr
    );
}

#[test]
fn explore_returns_nonempty_structured_output() {
    let dir = TestDir::new("explore");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(dir.path(), &["explore", "Counter", "-p", p]);
    assert!(run.ok, "explore must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("## Exploration: Counter"),
        "explore output must carry the exploration header: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Counter"),
        "explore output must mention the queried symbol: {}",
        run.stdout
    );
}

#[test]
fn explore_json_envelope_is_valid() {
    let dir = TestDir::new("explore-json");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(dir.path(), &["explore", "Counter", "-p", p, "--json"]);
    assert!(run.ok, "explore --json must succeed: {}", run.stderr);
    let v: serde_json::Value = serde_json::from_str(&run.stdout).expect("explore emits JSON");
    assert_eq!(v["command"], serde_json::json!("explore"));
    assert_eq!(v["isError"], serde_json::json!(false));
    assert!(
        v["output"].as_str().unwrap().contains("Exploration"),
        "explore JSON output must carry the rendered text: {}",
        run.stdout
    );
}

#[test]
fn node_symbol_returns_source_and_trail() {
    let dir = TestDir::new("node-symbol");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(dir.path(), &["node", "Counter", "-p", p]);
    assert!(run.ok, "node <symbol> must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("Counter"),
        "node output must name the symbol: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Location:"),
        "node symbol output must carry the location line: {}",
        run.stdout
    );
}

#[test]
fn node_file_returns_numbered_source() {
    let dir = TestDir::new("node-file");
    let project = indexed_project(&dir);
    let p = project.to_str().unwrap();

    let run = run_in(dir.path(), &["node", "src/math.ts", "-p", p]);
    assert!(run.ok, "node <file> must succeed: {}", run.stderr);
    assert!(
        run.stdout.contains("src/math.ts"),
        "node file output must name the file: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("export function add"),
        "node file output must include the verbatim source: {}",
        run.stdout
    );
}
