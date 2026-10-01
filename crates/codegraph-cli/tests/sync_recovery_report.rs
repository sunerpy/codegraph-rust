//! A sync that heals an interrupted index says so (upstream v1.6.1 #1360): the
//! orphan sweep's work is reported instead of reading as a no-op.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_core::types::{EdgeKind, Language, UnresolvedRef};
use codegraph_store::Store;

struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
fn a_healing_sync_reports_the_pending_references_it_resolved() {
    let dir = TestDir(std::env::temp_dir().join(format!(
        "codegraph-cli-sync-recovery-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )));
    fs::create_dir_all(dir.0.join("src")).unwrap();
    fs::write(
        dir.0.join("src/math.ts"),
        "export function add(a: number, b: number): number { return a + b; }\n",
    )
    .unwrap();
    fs::write(
        dir.0.join("src/app.ts"),
        "import { add } from './math';\nexport function run(): number { return add(1, 2); }\n",
    )
    .unwrap();
    let path = dir.0.to_str().unwrap();
    let (stdout, stderr, ok) = run(&dir.0, &["init", path]);
    assert!(ok, "init failed: {stdout} {stderr}");

    let (stdout, stderr, ok) = run(&dir.0, &["sync", path]);
    assert!(ok, "sync failed: {stdout} {stderr}");
    assert!(
        !stdout.contains("pending references"),
        "a healthy sync sweeps nothing: {stdout}"
    );

    // Simulate an interrupted resolution pass: the resolved call edge is gone,
    // its reference is parked again, and the incomplete-resolution marker is set.
    {
        let mut store = Store::open(&dir.0.join(".codegraph").join("codegraph.db")).unwrap();
        let caller = store
            .nodes_by_file_path("src/app.ts")
            .unwrap()
            .into_iter()
            .find(|node| node.name == "run")
            .expect("run node");
        store.delete_resolved_edges_from_file("src/app.ts").unwrap();
        store
            .insert_unresolved_refs(&[
                UnresolvedRef {
                    id: None,
                    from_node_id: caller.id.clone(),
                    reference_name: "add".to_string(),
                    reference_kind: EdgeKind::Calls,
                    line: 2,
                    col: 0,
                    candidates: None,
                    file_path: "src/app.ts".to_string(),
                    language: Language::TypeScript,
                    is_function_ref: false,
                    reference_subkind: None,
                },
                UnresolvedRef {
                    id: None,
                    from_node_id: caller.id,
                    reference_name: "missingEverywhere".to_string(),
                    reference_kind: EdgeKind::Calls,
                    line: 2,
                    col: 0,
                    candidates: None,
                    file_path: "src/app.ts".to_string(),
                    language: Language::TypeScript,
                    is_function_ref: false,
                    reference_subkind: None,
                },
            ])
            .unwrap();
        store.set_resolution_incomplete().unwrap();
    }

    let (stdout, stderr, ok) = run(&dir.0, &["sync", path]);
    assert!(ok, "sync failed: {stdout} {stderr}");
    assert!(
        stdout.contains("Resolved 1 pending references (1 unresolved)"),
        "stdout={stdout}"
    );
}
