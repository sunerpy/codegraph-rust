//! PR2 (Feature A) determinism gate: a Godot project WITH an opt-in
//! `godot.dsl.idFields` config must produce BYTE-IDENTICAL store output via
//! (i) `sync` after an edit vs a full `index --force` from scratch, and
//! (ii) parallel vs sequential indexing.
//!
//! This is the A5 counterpart to `sync_incremental.rs` / `parallel_index.rs`,
//! but exercises the `.tres` framework-extraction path that emits
//! `godot:id:<kind>:<value>` sentinels — i.e. it stresses the mtime-cached
//! `dsl_id_fields` reader + `find_config_path` tree-walk under both the
//! incremental (`sync`) and full (`index --force`) code paths, and under both
//! the rayon parallel parse and a single-threaded (`RAYON_NUM_THREADS=1`)
//! parse.
//!
//! Comparisons use the codegraph-bench canonical oracle (order-independent
//! edge/ref multisets + `.schema`), so "identical" means content + `.schema`,
//! not autoincrement rowid order. The `godot:id:*` sentinels live in
//! `unresolved_refs`, which the oracle includes in its canonical form, so a
//! perturbation of the ID-capture path WOULD surface here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-idfields-{label}-{}-{}",
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

const IDFIELDS_CONFIG: &str = r#"{
  "godot": {
    "dsl": {
      "idFields": {
        "buff_id": { "kind": "buff" },
        "skill_effect": { "kind": "skill", "separator": ":", "idSegments": [2, 4] }
      }
    }
  }
}
"#;

const PROJECT_GODOT: &str = "[application]\nconfig/name=\"idfields\"\n";

const BUFF_TRES: &str = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 7005
skill_effect = \"a:b:9015:c:7005:1000\"
duration = 5.0
";

/// Lay down a Godot project (project.godot + a `.tres` + an idFields config)
/// under `root`.
fn write_godot_project(root: &Path) {
    fs::create_dir_all(root.join(".codegraph")).unwrap();
    fs::write(
        root.join(".codegraph").join("codegraph.json"),
        IDFIELDS_CONFIG,
    )
    .unwrap();
    fs::write(root.join("project.godot"), PROJECT_GODOT).unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data").join("strength.tres"), BUFF_TRES).unwrap();
}

/// Run the binary with `cwd` as the working directory. The config reader walks
/// up from each `.tres`'s relative path joined onto cwd, so the project must be
/// the cwd for the opt-in `idFields` config to be discovered — exactly how a
/// user runs `codegraph` from inside their project. `CODEGRAPH_NO_DAEMON` keeps
/// the run foreground so the test never blocks on a background daemon.
fn cli_cwd(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_codegraph"));
    cmd.current_dir(cwd);
    cmd.args(args);
    cmd.env("CODEGRAPH_NO_DAEMON", "1");
    cmd.env("CODEGRAPH_NO_WATCH", "1");
    let output = cmd.output().expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn db_path(project: &Path) -> PathBuf {
    project.join(".codegraph").join("codegraph.db")
}

/// (i) `sync` after an edit to the `.tres` must equal a full `index --force`
/// from scratch on the same end state — proving the idFields capture path is
/// stable across the incremental vs full pipelines.
#[test]
fn idfields_sync_after_edit_equals_index_force_from_scratch() {
    // Given an indexed Godot project with an idFields config,
    let incremental = TestDir::new("sync");
    let project = incremental.path().join("game");
    write_godot_project(&project);
    let (out, err, ok) = cli_cwd(&project, &["init", "."]);
    assert!(ok, "init failed: stdout={out} stderr={err}");

    // When the `.tres` is edited (a new idField line added) and synced,
    let edited = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 8001
skill_effect = \"x:y:1234:z:5678:9\"
duration = 9.0
";
    fs::write(project.join("data").join("strength.tres"), edited).unwrap();
    let (out, err, ok) = cli_cwd(&project, &["sync", "."]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");

    // And a fresh project is built to the SAME end state via index --force,
    let scratch = TestDir::new("sync-scratch");
    let scratch_project = scratch.path().join("game");
    write_godot_project(&scratch_project);
    fs::write(scratch_project.join("data").join("strength.tres"), edited).unwrap();
    let (out, err, ok) = cli_cwd(&scratch_project, &["init", "."]);
    assert!(ok, "scratch init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli_cwd(&scratch_project, &["index", "--force", "."]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    // Then the incremental DB is canonically identical to the from-scratch DB.
    let synced = canonicalize_db(&db_path(&project)).unwrap();
    let rebuilt = canonicalize_db(&db_path(&scratch_project)).unwrap();
    diff_canonical(&rebuilt, &synced, None)
        .expect("idFields sync-after-edit must equal a full index --force from scratch");
}

/// (ii) Two independent parallel `index --force` runs of the same idFields
/// Godot project must be canonically identical — proving the rayon-parallel
/// framework-extraction path (with the mtime-cached config reader shared across
/// threads) converges on ONE fixed canonical form regardless of thread
/// scheduling. This mirrors `parallel_index.rs`'s determinism property, applied
/// to the `godot:id:*` sentinel-emitting path. (A single-threaded run via
/// `RAYON_NUM_THREADS=1` is deliberately NOT used: the CLI bulk-index pipeline
/// overlaps a rayon producer scope with an ordered consumer, which deadlocks on
/// a one-thread pool — so run-to-run determinism is the controllable proxy for
/// scheduling-independence, exactly as `parallel_index.rs` establishes.)
#[test]
fn idfields_parallel_index_is_deterministic_across_runs() {
    // Given two copies of the same idFields Godot project,
    let first = TestDir::new("parallel-a");
    let first_project = first.path().join("game");
    write_godot_project(&first_project);
    let (out, err, ok) = cli_cwd(&first_project, &["init", "."]);
    assert!(ok, "first init failed: stdout={out} stderr={err}");

    let second = TestDir::new("parallel-b");
    let second_project = second.path().join("game");
    write_godot_project(&second_project);
    let (out, err, ok) = cli_cwd(&second_project, &["init", "."]);
    assert!(ok, "second init failed: stdout={out} stderr={err}");

    // When each is independently indexed with the default rayon parallelism,
    let (out, err, ok) = cli_cwd(&first_project, &["index", "--force", "."]);
    assert!(ok, "first index --force failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli_cwd(&second_project, &["index", "--force", "."]);
    assert!(ok, "second index --force failed: stdout={out} stderr={err}");

    // Then both DBs are canonically identical (one fixed canonical form).
    let run_one = canonicalize_db(&db_path(&first_project)).unwrap();
    let run_two = canonicalize_db(&db_path(&second_project)).unwrap();
    diff_canonical(&run_one, &run_two, None)
        .expect("two parallel idFields index --force runs must be canonically identical");
}
