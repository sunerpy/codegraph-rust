//! Runtime contract for projects containing only unsupported source languages.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-unsupported-{label}-{}-{}",
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

fn init(project: &Path) -> (bool, String, String) {
    let output = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph init");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn unsupported_only_project_reports_extensions_and_inactive_state() {
    let root = TestDir::new("only");
    fs::write(root.0.join("a.move"), "module a {}\n").unwrap();
    fs::write(root.0.join("b.MOVE"), "module b {}\n").unwrap();
    fs::write(root.0.join("tool.pl"), "print 1;\n").unwrap();

    let (ok, stdout, stderr) = init(&root.0);
    assert!(ok, "init failed: stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("No supported source files found")
            && stdout.contains("3 file(s) present")
            && stdout.contains(".move (2)")
            && stdout.contains(".pl (1)"),
        "extension summary missing: {stdout}"
    );
    assert!(
        stdout.contains("CodeGraph is inactive for this workspace")
            && stdout.contains("searches will return nothing"),
        "inactive state missing: {stdout}"
    );
}

#[test]
fn supported_or_empty_projects_keep_their_existing_messages() {
    let supported = TestDir::new("mixed");
    fs::write(supported.0.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(supported.0.join("ignored.move"), "module ignored {}\n").unwrap();
    let (ok, stdout, stderr) = init(&supported.0);
    assert!(ok, "init failed: stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("Indexed 1 files"), "got: {stdout}");
    assert!(
        !stdout.contains("inactive for this workspace"),
        "got: {stdout}"
    );

    let empty = TestDir::new("empty");
    let (ok, stdout, stderr) = init(&empty.0);
    assert!(ok, "init failed: stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("No files found to index"), "got: {stdout}");
    assert!(
        !stdout.contains("inactive for this workspace"),
        "got: {stdout}"
    );
}
