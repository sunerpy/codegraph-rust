//! Honest warning for a supported file whose parse tree has errors and produces no symbols (#1522).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-parse-warning-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source(delimiter: &str) -> String {
    format!(
        "const char* kTemplate = R\"{delimiter}(\nstruct Ignored {{ int v; }};\n){delimiter}\";\n\nint after_the_raw_string(int x) {{\n  return x + 1;\n}}\n"
    )
}

fn run(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run codegraph");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn collapsed_parse_warns_without_failing_and_healthy_reindex_clears_it() {
    let dir = TestDir::new();
    fs::write(dir.0.join("min.cpp"), source("FILE_TEMPLATE_V1")).unwrap();
    let path = dir.0.to_str().unwrap();

    let (stdout, stderr, ok) = run(&dir.0, &["init", path]);
    assert!(
        ok,
        "warning-only parse collapse must not fail: {stdout} {stderr}"
    );
    assert!(stdout.contains("Indexed 1 files"), "stdout={stdout}");
    assert!(
        stderr.contains("min.cpp: parse produced no symbols (tree has errors)"),
        "stderr={stderr}"
    );

    fs::write(dir.0.join("min.cpp"), source("FILE_TEMPLATE_V")).unwrap();
    let (stdout, stderr, ok) = run(&dir.0, &["index", "--force", path]);
    assert!(ok, "healthy reindex failed: {stdout} {stderr}");
    assert!(
        !stderr.contains("parse produced no symbols"),
        "stderr={stderr}"
    );

    let (stdout, stderr, ok) = run(
        &dir.0,
        &["search", "after_the_raw_string", "-p", path, "--json"],
    );
    assert!(ok, "search failed: {stdout} {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value[0]["node"]["name"], "after_the_raw_string");

    fs::write(dir.0.join("min.cpp"), source("FILE_TEMPLATE_V1")).unwrap();
    let (stdout, stderr, ok) = run(&dir.0, &["sync", path]);
    assert!(ok, "warning-only sync must not fail: {stdout} {stderr}");
    assert!(
        stderr.contains("min.cpp: parse produced no symbols (tree has errors)"),
        "sync must surface the persisted warning: stderr={stderr}"
    );
}
