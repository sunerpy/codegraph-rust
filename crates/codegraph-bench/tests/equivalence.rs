use std::path::{Path, PathBuf};

use codegraph_bench::oracle::{
    assert_equivalent, canonicalize_db, diff_canonical, load_golden, write_golden, Tier,
};
use serde_json::json;

#[test]
fn generated_golden_matches_committed_mini_fixture() {
    let tempdir = TestDir::new("generated-golden");
    write_golden(&mini_db(), tempdir.path()).unwrap();

    let expected = load_golden(&mini_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn upstream_db_is_self_equivalent_to_mini_golden() {
    assert_equivalent(&mini_db(), &mini_golden_dir()).unwrap();
}

#[test]
fn tier1_node_drift_is_reported() {
    let expected = load_golden(&mini_golden_dir()).unwrap();
    let mut actual = expected.clone();
    actual.nodes[0].insert("name".to_string(), json!("DRIFTED_NAME"));

    let error = diff_canonical(&expected, &actual, None).unwrap_err();
    println!("injected Tier-1 drift failure:\n{error}");

    assert!(error
        .entries()
        .iter()
        .any(|entry| entry.tier == Tier::Tier1 && entry.surface == "nodes"));
}

#[test]
fn tier2_edges_are_order_independent_but_counted() {
    let expected = canonicalize_db(&mini_db()).unwrap();
    let mut reordered = expected.clone();
    reordered.edges.reverse();
    diff_canonical(&expected, &reordered, None).unwrap();

    let mut missing = expected.clone();
    let removed = missing.edges.pop().expect("mini fixture has edges");
    let error = diff_canonical(&expected, &missing, None).unwrap_err();
    println!("removed edge for Tier-2 missing-edge assertion: {removed:?}");
    println!("missing-edge failure:\n{error}");

    assert!(error
        .entries()
        .iter()
        .any(|entry| entry.tier == Tier::Tier2 && entry.surface == "edges"));
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-bench is under crates/")
        .to_path_buf()
}

fn mini_db() -> PathBuf {
    workspace_root().join("reference/golden/mini/colby.db")
}

fn mini_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/mini")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-bench-equivalence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
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
