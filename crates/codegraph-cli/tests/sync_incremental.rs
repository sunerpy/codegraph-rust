use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};

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
            "codegraph-cli-sync-{label}-{}-{}",
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

fn db_path(project: &Path) -> PathBuf {
    project.join(".codegraph").join("codegraph.db")
}

#[test]
fn sync_after_single_edit_equals_index_force_from_scratch() {
    let fixture = mini_fixture();

    let incremental = TestDir::new("incremental");
    let project = incremental.path().join("mini");
    copy_tree(&fixture, &project);

    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let edited = "export function add(left: number, right: number): number {\n  return left + right + 0;\n}\n\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n\n  decrement(step: number = 1): number {\n    this.value = add(this.value, -step);\n    return this.value;\n  }\n}\n";
    fs::write(project.join("src/math.ts"), edited).unwrap();

    let (out, err, ok) = cli(&["sync", project.to_str().unwrap()]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");
    assert!(
        out.contains("1 reindexed"),
        "expected exactly one file reindexed, got: {out}"
    );

    let scratch = TestDir::new("scratch");
    let scratch_project = scratch.path().join("mini");
    copy_tree(&fixture, &scratch_project);
    fs::write(scratch_project.join("src/math.ts"), edited).unwrap();

    let (out, err, ok) = cli(&["init", scratch_project.to_str().unwrap()]);
    assert!(ok, "scratch init failed: stdout={out} stderr={err}");

    let (out, err, ok) = cli(&["index", "--force", scratch_project.to_str().unwrap()]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    let synced = canonicalize_db(&db_path(&project)).unwrap();
    let rebuilt = canonicalize_db(&db_path(&scratch_project)).unwrap();

    diff_canonical(&rebuilt, &synced, None)
        .expect("incremental sync DB must equal a full index --force from scratch");
}

fn assert_sync_equals_index_force(label: &str, baseline: impl Fn(&Path), mutate: impl Fn(&Path)) {
    let fixture = mini_fixture();

    let incremental = TestDir::new(label);
    let project = incremental.path().join("mini");
    copy_tree(&fixture, &project);
    baseline(&project);
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    mutate(&project);
    let (out, err, ok) = cli(&["sync", project.to_str().unwrap()]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");

    let scratch = TestDir::new(&format!("{label}-scratch"));
    let scratch_project = scratch.path().join("mini");
    copy_tree(&fixture, &scratch_project);
    baseline(&scratch_project);
    mutate(&scratch_project);
    let (out, err, ok) = cli(&["init", scratch_project.to_str().unwrap()]);
    assert!(ok, "scratch init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", scratch_project.to_str().unwrap()]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    let synced = canonicalize_db(&db_path(&project)).unwrap();
    let rebuilt = canonicalize_db(&db_path(&scratch_project)).unwrap();
    diff_canonical(&rebuilt, &synced, None)
        .unwrap_or_else(|e| panic!("[{label}] incremental sync must equal index --force: {e:?}"));
}

#[test]
fn sync_body_only_edit_equals_index_force() {
    assert_sync_equals_index_force(
        "body-edit",
        |_| {},
        |project| {
            let edited = "export function add(left: number, right: number): number {\n  return left + right + 0;\n}\n\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n}\n";
            fs::write(project.join("src/math.ts"), edited).unwrap();
        },
    );
}

#[test]
fn sync_add_symbol_referenced_by_unchanged_file_equals_index_force() {
    let app_uses_multiply = "import { Counter, add, multiply } from './math';\n\nexport function runDemo(): number {\n  const counter = new Counter();\n  counter.increment(add(1, 2));\n  counter.increment(multiply(3, 4));\n  return counter.increment();\n}\n\nrunDemo();\n";
    assert_sync_equals_index_force(
        "add-symbol",
        |project| {
            fs::write(project.join("src/app.ts"), app_uses_multiply).unwrap();
        },
        |project| {
            let math = "export function add(left: number, right: number): number {\n  return left + right;\n}\n\nexport function multiply(left: number, right: number): number {\n  return left * right;\n}\n\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n}\n";
            fs::write(project.join("src/math.ts"), math).unwrap();
        },
    );
}

#[test]
fn sync_remove_referenced_symbol_equals_index_force() {
    assert_sync_equals_index_force(
        "remove-symbol",
        |_| {},
        |project| {
            let math = "export class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = this.value + step;\n    return this.value;\n  }\n}\n";
            fs::write(project.join("src/math.ts"), math).unwrap();
        },
    );
}

#[test]
fn sync_rename_referenced_symbol_equals_index_force() {
    assert_sync_equals_index_force(
        "rename-symbol",
        |_| {},
        |project| {
            let math = "export function sum(left: number, right: number): number {\n  return left + right;\n}\n\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = sum(this.value, step);\n    return this.value;\n  }\n}\n";
            fs::write(project.join("src/math.ts"), math).unwrap();
        },
    );
}

#[test]
fn sync_with_no_changes_reindexes_zero_files() {
    let dir = TestDir::new("unchanged");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    let (out, err, ok) = cli(&["sync", project.to_str().unwrap()]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");
    assert!(
        out.contains("0 reindexed") && out.contains("3 skipped"),
        "unchanged sync must reindex 0 and skip all 3 files, got: {out}"
    );
}

#[test]
fn sync_after_file_removal_equals_index_force() {
    let fixture = mini_fixture();

    let incremental = TestDir::new("removal");
    let project = incremental.path().join("mini");
    copy_tree(&fixture, &project);

    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    fs::remove_file(project.join("tools/greeter.py")).unwrap();

    let (out, err, ok) = cli(&["sync", project.to_str().unwrap()]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");
    assert!(
        out.contains("1 removed"),
        "expected exactly one file removed, got: {out}"
    );

    let scratch = TestDir::new("removal-scratch");
    let scratch_project = scratch.path().join("mini");
    copy_tree(&fixture, &scratch_project);
    fs::remove_file(scratch_project.join("tools/greeter.py")).unwrap();

    let (out, err, ok) = cli(&["init", scratch_project.to_str().unwrap()]);
    assert!(ok, "scratch init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", scratch_project.to_str().unwrap()]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    let synced = canonicalize_db(&db_path(&project)).unwrap();
    let rebuilt = canonicalize_db(&db_path(&scratch_project)).unwrap();

    diff_canonical(&rebuilt, &synced, None)
        .expect("sync after file removal must equal a full index --force from scratch");
}
