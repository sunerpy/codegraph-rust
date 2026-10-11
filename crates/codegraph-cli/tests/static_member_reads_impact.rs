//! A type used only through a static field or enum value has its readers as
//! dependents (G4, upstream `extraction.test.ts` "links a type referenced only
//! via a static field / enum value"), and a read never links a same-named
//! type of another language family.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-static-member-{label}-{}-{}",
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

/// The files of the symbols `impact` reports for `symbol`.
fn impacted_files(project: &Path, symbol: &str) -> Vec<String> {
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");
    let (stdout, err, ok) = cli(&["impact", symbol, "-p", p, "--depth", "3", "--json"]);
    assert!(ok, "impact failed: stdout={stdout} stderr={err}");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("impact emits valid JSON on stdout");
    value["affected"]
        .as_array()
        .expect("affected is an array")
        .iter()
        .filter_map(|node| node["filePath"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn a_type_read_only_through_a_static_field_has_its_reader_as_dependent() {
    let dir = TestDir::new("java");
    let project = dir.path().join("java");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("JsonScope.java"),
        "class JsonScope {\n  static final int EMPTY_DOCUMENT = 1;\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("Reader.java"),
        "class Reader {\n  private int helper;\n  int peek() {\n    return JsonScope.EMPTY_DOCUMENT;\n  }\n  int noop() {\n    return this.helper;\n  }\n}\n",
    )
    .unwrap();
    let files = impacted_files(&project, "JsonScope");
    assert!(
        files.iter().any(|path| path.ends_with("Reader.java")),
        "{files:?}"
    );
}

#[test]
fn a_static_member_read_never_links_another_language_family() {
    let dir = TestDir::new("cross");
    let project = dir.path().join("cross");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("Build.ts"),
        "export class Build {\n  static version = 1;\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("Device.kt"),
        "package app\nclass Device {\n  fun sdk(): Int = Build.VERSION\n}\n",
    )
    .unwrap();
    let files = impacted_files(&project, "Build");
    assert!(
        !files.iter().any(|path| path.ends_with("Device.kt")),
        "{files:?}"
    );
}
