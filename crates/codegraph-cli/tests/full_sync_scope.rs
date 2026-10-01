//! A full `sync` keeps exactly the files `index` keeps.
//!
//! The watcher deliberately prunes some directories the scan indexes: the
//! watch-only defaults such as `.cache`, and every top-level `.codegraph-*`
//! directory. A full sync used to judge scope with that stricter watch policy,
//! so `sync` deleted files `index` had just indexed, and `status` then reported
//! them as pending additions — `sync` no longer equalled `index --force`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-full-sync-scope-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    assert!(
        output.status.success(),
        "codegraph {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn touch(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn indexed_paths(project: &str) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(&cli(&["files", "-p", project, "--json"])).expect("files JSON");
    let mut paths: Vec<String> = value
        .as_array()
        .expect("files array")
        .iter()
        .map(|file| file["path"].as_str().expect("path").to_string())
        .collect();
    paths.sort();
    paths
}

fn pending_total(project: &str) -> u64 {
    let value: serde_json::Value =
        serde_json::from_str(&cli(&["status", project, "--json"])).expect("status JSON");
    let pending = &value["pendingChanges"];
    ["added", "modified", "removed"]
        .iter()
        .map(|key| pending[*key].as_u64().expect("pending count"))
        .sum()
}

#[test]
fn full_sync_keeps_files_the_watcher_does_not_watch() {
    let dir = TestDir::new("watch-only");
    let project = dir.path.join("proj");
    touch(&project, "src/app.ts", "export const app = 1;\n");
    // `.cache` and `vcpkg_installed` are watch-only ignores; the scan indexes
    // them. A top-level `.codegraph-*` directory that is not the index root is
    // scanned too, but the watcher rejects it.
    touch(&project, ".cache/cached.ts", "export const cached = 1;\n");
    touch(
        &project,
        "vcpkg_installed/port.ts",
        "export const port = 1;\n",
    );
    touch(
        &project,
        ".codegraph-sources/kept.ts",
        "export const kept = 1;\n",
    );
    let p = project.to_str().unwrap();

    cli(&["init", p]);
    let indexed = indexed_paths(p);
    assert_eq!(
        indexed,
        vec![
            ".cache/cached.ts".to_string(),
            ".codegraph-sources/kept.ts".to_string(),
            "src/app.ts".to_string(),
            "vcpkg_installed/port.ts".to_string(),
        ]
    );
    assert_eq!(pending_total(p), 0, "a fresh index has nothing pending");

    cli(&["sync", p]);
    assert_eq!(indexed_paths(p), indexed, "sync must keep what index keeps");
    assert_eq!(pending_total(p), 0, "status must agree with sync");

    cli(&["index", "--force", p]);
    assert_eq!(indexed_paths(p), indexed, "sync equals index --force");
}
