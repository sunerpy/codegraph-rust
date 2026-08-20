//! A mutating command must not silently retarget an ANCESTOR index (#1524).
//!
//! `index .` inside `parent/child`, with only `parent/` indexed, walks up and
//! rebuilds the PARENT — reporting a file count that includes the parent's own
//! files while creating no child index, and printing nothing about it. Upstream's
//! thread widens the diagnostic to `sync` / `uninit` / `unlock`.
//!
//! Each test drives the REAL binary, because the defect is in the CLI's project
//! resolution, and asserts on STDERR: a mutating command's stdout is parsed by
//! scripts, and `stdout_purity.rs` pins that contract.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-ancestor-{label}-{}-{}",
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

/// Run the binary with `cwd` as its working directory, so a bare `.` argument
/// exercises the same resolution an interactive user hits.
fn cli_in(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
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

/// `parent/` indexed, `parent/child/` NOT indexed — the #1524 shape.
fn parent_indexed_child_not(dir: &TestDir) -> (PathBuf, PathBuf) {
    let parent = dir.path().join("parent");
    let child = parent.join("child");
    fs::create_dir_all(&child).unwrap();
    fs::write(parent.join("parent_file.ts"), "export const p = 1;\n").unwrap();
    fs::write(child.join("child_file.ts"), "export const c = 2;\n").unwrap();
    let (out, err, ok) = cli_in(dir.path(), &["init", parent.to_str().unwrap()]);
    assert!(ok, "init parent failed: stdout={out} stderr={err}");
    (parent, child)
}

/// The parent path is a PREFIX of the child path, and an unrelated warning (the
/// stdio-MCP index-holder pre-warning) already prints the resolved project — so
/// asserting "stderr mentions the parent" would pass without any retarget
/// diagnostic at all. The assertion therefore isolates the warning's own LINE
/// and requires both paths within it.
fn assert_retarget_warning(stderr: &str, parent: &Path, child: &Path) {
    let line = stderr
        .lines()
        .find(|l| l.contains("resolved to an ancestor"))
        .unwrap_or_else(|| {
            panic!("no ancestor-retarget warning line in stderr:\n{stderr}");
        });
    assert!(
        line.contains(&child.display().to_string()),
        "the warning must name the directory the user asked for: {line}"
    );
    assert!(
        line.contains(&parent.display().to_string()),
        "the warning must name the ANCESTOR actually operated on: {line}"
    );
    assert!(
        stderr.contains("codegraph init"),
        "the warning must say how to get a child-local index: {stderr}"
    );
}

#[test]
fn index_in_an_unindexed_child_warns_that_the_parent_is_the_target() {
    let dir = TestDir::new("index");
    let (parent, child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&child, &["index", "."]);
    assert!(
        ok,
        "index must still succeed: stdout={stdout} stderr={stderr}"
    );
    assert_retarget_warning(&stderr, &parent, &child);
}

#[test]
fn sync_in_an_unindexed_child_warns() {
    let dir = TestDir::new("sync");
    let (parent, child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&child, &["sync", "."]);
    assert!(
        ok,
        "sync must still succeed: stdout={stdout} stderr={stderr}"
    );
    assert_retarget_warning(&stderr, &parent, &child);
}

#[test]
fn uninit_in_an_unindexed_child_warns_before_deleting_the_parent_index() {
    // The most destructive case: `uninit --force` in a child deletes the
    // PARENT's index. A silent retarget here removes an index the user never
    // named.
    let dir = TestDir::new("uninit");
    let (parent, child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&child, &["uninit", ".", "--force"]);
    assert!(
        ok,
        "uninit must still succeed: stdout={stdout} stderr={stderr}"
    );
    assert_retarget_warning(&stderr, &parent, &child);
}

#[test]
fn unlock_in_an_unindexed_child_warns() {
    let dir = TestDir::new("unlock");
    let (parent, child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&child, &["unlock", "."]);
    assert!(
        ok,
        "unlock must still succeed: stdout={stdout} stderr={stderr}"
    );
    assert_retarget_warning(&stderr, &parent, &child);
}

#[test]
fn index_at_the_indexed_root_itself_warns_about_nothing() {
    // The control. This is the overwhelmingly common case, and a warning here
    // would be noise on every single run — so the check must key on "the
    // resolved project differs from what was asked for", not on "an ancestor
    // walk happened".
    let dir = TestDir::new("noop");
    let (parent, _child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&parent, &["index", "."]);
    assert!(ok, "index failed: stdout={stdout} stderr={stderr}");
    assert!(
        !stderr.contains("resolved to an ancestor"),
        "operating on the requested directory must not warn: {stderr}"
    );
}

#[test]
fn the_retarget_warning_never_reaches_stdout() {
    // `stdout_purity.rs` pins machine-readable stdout for these commands; a
    // warning printed there would corrupt any script parsing the output.
    let dir = TestDir::new("purity");
    let (_parent, child) = parent_indexed_child_not(&dir);
    let (stdout, stderr, ok) = cli_in(&child, &["index", "."]);
    assert!(ok, "index failed: stderr={stderr}");
    assert!(
        !stdout.contains("resolved to an ancestor"),
        "the warning must be on stderr only, found on stdout: {stdout}"
    );
}
