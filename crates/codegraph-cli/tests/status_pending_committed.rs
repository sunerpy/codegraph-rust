//! `status` must report committed-but-unindexed work (#1829).
//!
//! The detector is intentionally Git-independent (scope-aware full scan), but a
//! clean working tree after commit is the regression shape that previously made
//! fixed-zero / working-tree-only status implementations lie.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codegraph-status-committed-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".gitignore"), ".codegraph/\n").unwrap();
        fs::write(
            root.join("src/modify.ts"),
            "export function before(): number { return 1; }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/remove.ts"),
            "export function removeMe(): number { return 2; }\n",
        )
        .unwrap();
        git(&root, &["init"]);
        git(
            &root,
            &["config", "user.email", "codegraph@example.invalid"],
        );
        git(&root, &["config", "user.name", "CodeGraph Test"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-m", "initial"]);
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Repository-locating variables (`git rev-parse --local-env-vars`). Git
/// exports them to hooks, so a test run from `pre-push` would otherwise commit
/// its throwaway fixture into the repository being pushed.
const GIT_LOCAL_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

fn git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for name in GIT_LOCAL_ENV {
        command.env_remove(name);
    }
    let output = command
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn codegraph(root: &Path, args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .current_dir(root)
        .output()
        .expect("run codegraph");
    assert!(
        output.status.success(),
        "codegraph {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn cli_and_mcp_status_see_clean_committed_delta_without_mutating_the_index() {
    let project = Project::new();
    let root = project.path();
    codegraph(root, &["init", "."]);
    let db = root.join(".codegraph/codegraph.db");

    fs::write(
        root.join("src/modify.ts"),
        "export function afterCommit(): number { return 1000; }\n",
    )
    .unwrap();
    fs::remove_file(root.join("src/remove.ts")).unwrap();
    fs::write(
        root.join("src/add.ts"),
        "export function addedAfterIndex(): number { return 3; }\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("vendor")).unwrap();
    fs::write(
        root.join("vendor/ignored.ts"),
        "export function mustNotAppear(): number { return 4; }\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "committed delta"]);
    assert!(
        git(root, &["status", "--porcelain"]).trim().is_empty(),
        "the regression requires a clean working tree"
    );

    let before = fs::read(&db).unwrap();
    let value: Value =
        serde_json::from_str(&codegraph(root, &["status", ".", "--json"])).expect("status JSON");
    assert_eq!(
        value["pendingChanges"],
        json!({
            "added": 1,
            "modified": 1,
            "removed": 1,
            "addedPaths": ["src/add.ts"],
            "modifiedPaths": ["src/modify.ts"],
            "removedPaths": ["src/remove.ts"],
        })
    );

    let engine = codegraph_mcp::CodeGraphEngine::open(root).unwrap();
    let result = engine.execute("codegraph_status", &json!({}));
    let text = &result.content[0].text;
    assert!(text.contains("Pending sync: 1 added, 1 modified, 1 removed"));
    assert!(text.contains("added: src/add.ts"));
    assert!(text.contains("modified: src/modify.ts"));
    assert!(text.contains("removed: src/remove.ts"));
    assert!(!text.contains("vendor/ignored.ts"));
    drop(engine);
    assert_eq!(
        fs::read(&db).unwrap(),
        before,
        "CLI/MCP status must not mutate database bytes"
    );

    codegraph(root, &["sync", ".", "--quiet"]);
    let clean: Value = serde_json::from_str(&codegraph(root, &["status", ".", "--json"]))
        .expect("clean status JSON");
    assert_eq!(clean["pendingChanges"]["added"], 0);
    assert_eq!(clean["pendingChanges"]["modified"], 0);
    assert_eq!(clean["pendingChanges"]["removed"], 0);
    assert_eq!(clean["pendingChanges"]["addedPaths"], json!([]));
    assert_eq!(clean["pendingChanges"]["modifiedPaths"], json!([]));
    assert_eq!(clean["pendingChanges"]["removedPaths"], json!([]));
}
