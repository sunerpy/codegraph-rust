//! `status` reports indexed files whose symbols are missing although their
//! content is current (upstream #2336), and `files --json` carries each file's
//! recorded errors.
//!
//! A content-hash comparison calls such a file up to date, so before this
//! report `status` said "Index is up to date" while a file contributed nothing
//! to the graph. Two groups are reported:
//!
//! - files needing a re-index: rows stored with no nodes and no recorded
//!   reason, which a hash-based `sync` never revisits;
//! - files with parse errors: a recorded parse failure and no symbols from it.
//!
//! A file over the size limit and a file-level-only language are empty on
//! purpose and are in neither group.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-status-file-errors-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
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

fn run(args: &[&str], project: &Path) -> (String, String) {
    let output = Command::new(bin())
        .args(args)
        .arg(project)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env_remove("CODEGRAPH_DIR")
        .output()
        .expect("run codegraph");
    assert!(
        output.status.success(),
        "codegraph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A project with one healthy file, one whose parse collapses to no symbols,
/// one over the size limit, and a YAML file (a file-level-only language).
fn project(dir: &TestDir) -> PathBuf {
    let project = dir.0.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join(".codegraph")).unwrap();
    fs::write(
        project.join(".codegraph/config.toml"),
        "[app]\nname = \"status-file-errors\"\n\n[indexing]\nmax_file_size = 4096\n",
    )
    .unwrap();
    fs::write(
        project.join("src/healthy.ts"),
        "export function healthySymbol(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(project.join("src/broken.ts"), "@@@ ### ((( === }}} <<<\n").unwrap();
    fs::write(
        project.join("src/huge.ts"),
        "export const filler = 1;\n".repeat(400),
    )
    .unwrap();
    fs::write(project.join("settings.yaml"), "name: status\n").unwrap();
    run(&["init"], &project);
    project
}

/// Store `src/healthy.ts` the way a wiped row looks: no nodes, no error, and
/// the content hash still current.
fn wipe_row(project: &Path) {
    let db = project.join(".codegraph/codegraph.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let updated = conn
        .execute(
            "UPDATE files SET node_count = 0 WHERE path = 'src/healthy.ts'",
            [],
        )
        .unwrap();
    assert_eq!(updated, 1);
    conn.close().unwrap();
    for sidecar in ["-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{sidecar}", db.display()));
        assert!(
            !path.exists(),
            "closing the test's connection left {sidecar} behind"
        );
    }
}

#[test]
fn status_json_counts_files_missing_their_symbols() {
    let dir = TestDir::new("json");
    let project = project(&dir);

    let (stdout, _) = run(&["status", "--json"], &project);
    let status: Value = serde_json::from_str(&stdout).expect("status JSON");
    assert_eq!(status["index"]["filesNeedingReindex"], 0, "{status}");
    assert_eq!(
        status["index"]["filesWithParseErrors"], 1,
        "the collapsed parse is reported: {status}"
    );

    wipe_row(&project);
    let (stdout, _) = run(&["status", "--json"], &project);
    let status: Value = serde_json::from_str(&stdout).expect("status JSON");
    assert_eq!(
        status["index"]["filesNeedingReindex"], 1,
        "the wiped row is reported: {status}"
    );
    assert_eq!(status["index"]["filesWithParseErrors"], 1, "{status}");
    for key in [
        "builtWithVersion",
        "builtWithExtractionVersion",
        "currentExtractionVersion",
        "reindexRecommended",
    ] {
        assert!(
            status["index"].get(key).is_some(),
            "the existing `index.{key}` field stays: {status}"
        );
    }
}

#[test]
fn status_text_names_the_files_missing_their_symbols() {
    let dir = TestDir::new("text");
    let project = project(&dir);
    wipe_row(&project);

    let (stdout, _) = run(&["status"], &project);
    assert!(
        stdout.contains("1 file is missing its symbols") && stdout.contains("src/healthy.ts"),
        "the wiped row is named with what to do: {stdout}"
    );
    assert!(
        stdout.contains("1 file could not be parsed") && stdout.contains("src/broken.ts"),
        "the collapsed parse is named: {stdout}"
    );
    assert!(
        !stdout.contains("src/huge.ts") && !stdout.contains("settings.yaml"),
        "files that are empty on purpose are not reported: {stdout}"
    );
    assert!(
        !stdout.contains("Index is up to date"),
        "an index with files missing their symbols is not up to date: {stdout}"
    );
}

#[test]
fn files_json_carries_each_files_recorded_errors() {
    let dir = TestDir::new("files");
    let project = project(&dir);

    let (stdout, _) = run(&["files", "--json", "-p"], &project);
    let files: Vec<Value> = serde_json::from_str(&stdout).expect("files JSON");
    let errors_of = |path: &str| {
        files
            .iter()
            .find(|file| file["path"] == path)
            .unwrap_or_else(|| panic!("{path} is listed: {stdout}"))["errors"]
            .clone()
    };
    assert_eq!(errors_of("src/healthy.ts"), serde_json::json!([]));
    let broken = errors_of("src/broken.ts");
    assert!(
        broken
            .as_array()
            .is_some_and(|errors| errors.iter().any(|e| e
                .as_str()
                .is_some_and(|e| e.contains("parse produced no symbols")))),
        "the parse failure is listed: {broken}"
    );
    let huge = errors_of("src/huge.ts");
    assert!(
        huge.as_array().is_some_and(|errors| errors
            .iter()
            .any(|e| e.as_str().is_some_and(|e| e.contains("exceeds max size")))),
        "the size skip is listed: {huge}"
    );
}
