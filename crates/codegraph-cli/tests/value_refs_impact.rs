//! A constant's same-file readers are in its impact radius (upstream #895):
//! the value reference from a reader to the constant it reads is an incoming
//! edge `impact` follows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-value-refs-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn a_constants_same_file_readers_are_in_its_impact() {
    let dir = TestDir::new("impact");
    let project = dir.path().join("palette");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("palette.ts"),
        "export const COLOR_PALETTE = { red: \"#f00\", blue: \"#00f\" };\nexport function pickRed() { return COLOR_PALETTE.red; }\n",
    )
    .unwrap();
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    let (stdout, err, ok) = cli(&["impact", "COLOR_PALETTE", "-p", p, "--json"]);
    assert!(ok, "impact failed: stdout={stdout} stderr={err}");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("impact emits valid JSON on stdout");
    let affected: Vec<&str> = value["affected"]
        .as_array()
        .expect("affected is an array")
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert!(affected.contains(&"pickRed"), "{value}");
}
