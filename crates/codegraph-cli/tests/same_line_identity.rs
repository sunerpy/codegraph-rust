//! Same-line namesakes survive the store, not just extraction (#1349).
//!
//! Nodes are upserted by id, so two declarations sharing one id leave a single
//! row behind. The Vue extractor mints its script functions itself; this
//! indexes a real project and reads the rows back.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_core::types::NodeKind;
use codegraph_store::Store;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-same-line-identity-{label}-{}-{}",
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

fn cli(args: &[&str]) {
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
}

fn store(project: &Path) -> Store {
    let paths = IndexPaths::resolve(project, None).expect("resolve index paths");
    Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("open the index for reading")
}

#[test]
fn same_line_vue_functions_are_two_rows() {
    let dir = TestDir::new("vue");
    let project = dir.path.join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("Widget.vue"),
        "<template><div/></template>\n<script>\nfunction f() {} function f() {}\n</script>\n",
    )
    .unwrap();
    let p = project.to_str().unwrap();
    cli(&["init", p]);
    let rows = |project: &Path| {
        let mut rows = store(project)
            .all_nodes()
            .unwrap()
            .into_iter()
            .filter(|node| node.kind == NodeKind::Function && node.name == "f")
            .map(|node| (node.id, node.start_line, node.start_column))
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    let indexed = rows(&project);
    assert_eq!(indexed.len(), 2, "{indexed:#?}");
    assert_ne!(indexed[0].0, indexed[1].0);
    cli(&["sync", p]);
    assert_eq!(rows(&project), indexed, "sync keeps both rows");
    cli(&["index", "--force", p]);
    assert_eq!(rows(&project), indexed, "index --force keeps both rows");
}
