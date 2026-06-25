//! End-to-end tests for the hidden `codegraph prompt-hook` subcommand.
//!
//! `prompt-hook` emits `codegraph_explore`-equivalent DETERMINISTIC retrieval
//! output (the SAME text the MCP explore tool produces — NO LLM/AI) to stdout
//! for a given query, resolving the nearest `.codegraph/` (monorepo-aware).
//! Mirrors the spawn-the-real-binary pattern of `sync_incremental.rs` /
//! `installer.rs` via `CARGO_BIN_EXE_codegraph`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-prompt-hook-{label}-{}-{}",
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
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// Run `prompt-hook` feeding the query over stdin (no positional/flag arg).
fn cli_stdin(args: &[&str], stdin_text: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn codegraph binary");
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(stdin_text.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait codegraph");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn index_mini(label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    (dir, project)
}

#[test]
fn prompt_hook_emits_explore_output() {
    let (_dir, project) = index_mini("emit");

    // Query a symbol known to exist in the mini fixture (src/math.ts:
    // `export class Counter`). The explore output must reference it. NO LLM is
    // involved — this is codegraph's own deterministic retrieval text.
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "Counter",
    ]);
    assert!(ok, "prompt-hook failed: stdout={out} stderr={err}");
    assert!(
        !out.trim().is_empty(),
        "expected non-empty stdout, got empty"
    );
    assert!(
        out.contains("Counter"),
        "expected explore output referencing the queried symbol, got:\n{out}"
    );
    // It must be explore-style content (the explore renderer's headers), proving
    // it is the same output the MCP explore tool produces.
    assert!(
        out.contains("Exploration:") || out.contains("Source Code"),
        "expected codegraph_explore-style content, got:\n{out}"
    );
}

#[test]
fn prompt_hook_reads_query_from_stdin() {
    let (_dir, project) = index_mini("stdin");

    let (out, err, ok) = cli_stdin(
        &["prompt-hook", "--path", project.to_str().unwrap()],
        "Counter",
    );
    assert!(ok, "prompt-hook (stdin) failed: stdout={out} stderr={err}");
    assert!(
        out.contains("Counter"),
        "expected stdin query to drive explore output, got:\n{out}"
    );
}

#[test]
fn prompt_hook_graceful_in_unindexed_dir() {
    let dir = TestDir::new("unindexed");
    let project = dir.path().join("empty");
    fs::create_dir_all(&project).unwrap();

    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "anything",
    ]);
    // Graceful: exit 0, no panic, a notice (not a crash / non-zero).
    assert!(
        ok,
        "prompt-hook in unindexed dir must exit 0; stdout={out} stderr={err}"
    );
    assert!(
        !err.contains("panicked"),
        "must not panic in an unindexed dir; stderr={err}"
    );
}

#[test]
fn prompt_hook_hidden_from_help() {
    // Hidden subcommand: it must NOT appear in the top-level help listing, but
    // must still be invocable (covered by the other tests).
    let (out, _err, ok) = cli(&["--help"]);
    assert!(ok, "help failed");
    assert!(
        !out.contains("prompt-hook"),
        "prompt-hook must be hidden from the main help, got:\n{out}"
    );
}
