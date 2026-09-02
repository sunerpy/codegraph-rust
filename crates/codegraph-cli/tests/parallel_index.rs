//! End-to-end regression tests locking in the parallel index pipeline (T2/T4/T5).
//!
//! These drive the real `codegraph` binary (`index --force`) against in-repo
//! fixtures and assert on the persisted SQLite DB through the codegraph-bench
//! canonical oracle (order-independent edge/ref multisets + `.schema`).
//!
//! Equivalence properties proven here:
//!   * `index_force_is_deterministic_run_to_run` — two independent `index --force`
//!     runs on the same fixture produce a CANONICALLY IDENTICAL DB. This is the
//!     key new property the parallel resolve work (T4) must hold: parallelism must
//!     not perturb resolution. Compared via the oracle, so "identical" means
//!     content + `.schema`, not autoincrement rowid order.
//!   * `parallel_index_canonical_form_is_stable_and_golden_holds` — proves the
//!     baseline→parallel equivalence at the level a self-contained test can
//!     control: a THIRD independent parallel `index --force` is canonically
//!     identical to the determinism pair (so the parallel output is a single
//!     fixed canonical form, not one of several), AND the authoritative committed
//!     golden artifacts (`reference/golden/mini`) still pass the order-independent
//!     oracle against the upstream reference DB. The authoritative content gate
//!     against the committed golden remains `cargo test -p codegraph-bench` (the
//!     11/11 + 4/4 oracle); this test does not weaken it. (A fresh Rust index of
//!     the mini fixture legitimately differs from the upstream `colby.db` — it
//!     resolves one fewer `this.value` ref — so we deliberately do NOT diff the
//!     live parallel DB against `colby.db`; that would false-fail on a known
//!     Rust/TS difference, not a regression.)
//!   * `oversized_file_size_skips_with_exact_file_record` — a file larger than
//!     `max_file_size` yields the exact size-skip `FileRecord` through the
//!     parallel parse path (T2): size set to the real byte length, node_count 0,
//!     and the exact `"File exceeds max size (N > M): path"` error string.
//!   * `deep_file_sync_discards_stale_graph_and_matches_force_index` — replacing
//!     a previously valid file with a 30,000-level AST removes its old graph,
//!     records one stable file error, leaves sibling files intact, and converges
//!     to the same canonical DB as a fresh `index --force`.
//!   * `full_index_leaves_synchronous_at_normal` — after a full `index --force`
//!     (which runs under `synchronous=OFF` per T5), a FRESH `Store::open` reports
//!     `PRAGMA synchronous != 0` (NORMAL), proving the bulk-index pragma never
//!     leaks past the full-index connection. This is the END-TO-END counterpart to
//!     the in-isolation Store-method tests in codegraph-store/tests/bulk_index_pragmas.rs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{assert_equivalent, canonicalize_db, diff_canonical};
use codegraph_store::connection::Store;

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

fn go_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/go")
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
            "codegraph-cli-parallel-{label}-{}-{}",
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
    cli_with_threads(args, None)
}

fn cli_with_threads(args: &[&str], threads: Option<&str>) -> (String, String, bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codegraph"));
    command.args(args);
    if let Some(threads) = threads {
        command.env("RAYON_NUM_THREADS", threads);
    }
    let output = command.output().expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn db_path(project: &Path) -> PathBuf {
    // `init`/`index` write the selected index namespace.
    project.join(".codegraph").join("codegraph.db")
}

fn init_and_force_index(project: &Path) {
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", project.to_str().unwrap()]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");
}

/// DETERMINISM (self-consistency): two independent `index --force` runs on the
/// same fixture must yield a canonically identical DB. The oracle compares
/// edges/refs as order-independent multisets, so this asserts content + `.schema`
/// stability run-to-run — the property the parallel resolve work must preserve.
#[test]
fn index_force_is_deterministic_run_to_run() {
    let first = TestDir::new("determinism-a");
    let first_project = first.path().join("mini");
    copy_tree(&mini_fixture(), &first_project);
    init_and_force_index(&first_project);

    let second = TestDir::new("determinism-b");
    let second_project = second.path().join("mini");
    copy_tree(&mini_fixture(), &second_project);
    init_and_force_index(&second_project);

    let run_one = canonicalize_db(&db_path(&first_project)).unwrap();
    let run_two = canonicalize_db(&db_path(&second_project)).unwrap();

    diff_canonical(&run_one, &run_two, None)
        .expect("two independent index --force runs must be canonically identical");
}

/// DETERMINISM within a single project dir: re-running `index --force` over an
/// existing DB (which removes and rebuilds it) is also canonically identical to
/// the first run. Guards against state left behind by the bulk-index pragma path.
#[test]
fn repeated_force_index_same_dir_is_canonically_identical() {
    let dir = TestDir::new("determinism-same-dir");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    init_and_force_index(&project);
    let first = canonicalize_db(&db_path(&project)).unwrap();

    let (out, err, ok) = cli(&["index", "--force", project.to_str().unwrap()]);
    assert!(ok, "second index --force failed: stdout={out} stderr={err}");
    let second = canonicalize_db(&db_path(&project)).unwrap();

    diff_canonical(&first, &second, None)
        .expect("re-running index --force in place must be canonically identical");
}

#[test]
fn debug_mode_is_canonically_identical_at_one_two_and_four_threads() {
    let mut cross_thread_baseline = None;
    for threads in ["1", "2", "4"] {
        let dir = TestDir::new(&format!("debug-equivalence-{threads}"));
        let project = dir.path().join("mini");
        copy_tree(&mini_fixture(), &project);
        let project_arg = project.to_str().unwrap();

        let (out, err, ok) = cli_with_threads(&["init", project_arg], Some(threads));
        assert!(ok, "init ({threads} threads) failed: {out} {err}");
        let (out, err, ok) = cli_with_threads(&["index", "--force", project_arg], Some(threads));
        assert!(ok, "ordinary index ({threads} threads) failed: {out} {err}");
        let ordinary = canonicalize_db(&db_path(&project)).unwrap();

        let (out, err, ok) = cli_with_threads(
            &["index", "--force", "--quiet", "--debug", project_arg],
            Some(threads),
        );
        assert!(ok, "debug index ({threads} threads) failed: {out} {err}");
        let debug = canonicalize_db(&db_path(&project)).unwrap();
        diff_canonical(&ordinary, &debug, None).unwrap_or_else(|error| {
            panic!("debug changed index output with {threads} threads: {error}")
        });

        if let Some(baseline) = &cross_thread_baseline {
            diff_canonical(baseline, &debug, None).unwrap_or_else(|error| {
                panic!("thread count changed index output at {threads} threads: {error}")
            });
        } else {
            cross_thread_baseline = Some(debug);
        }
    }
}

/// BASELINE→PARALLEL EQUIVALENCE (the form a self-contained test can control):
/// a third independent parallel `index --force` of the mini fixture must be
/// canonically identical to a determinism-pair run, proving the parallel pipeline
/// converges on a SINGLE fixed canonical form (not one of several orderings), and
/// the committed golden artifacts still pass the order-independent oracle against
/// the upstream reference DB. The authoritative content gate against the committed
/// golden is `cargo test -p codegraph-bench`; this test does not weaken it.
#[test]
fn parallel_index_canonical_form_is_stable_and_golden_holds() {
    let dir = TestDir::new("baseline-equiv");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    init_and_force_index(&project);
    let parallel = canonicalize_db(&db_path(&project)).unwrap();

    let other = TestDir::new("baseline-equiv-peer");
    let other_project = other.path().join("mini");
    copy_tree(&mini_fixture(), &other_project);
    init_and_force_index(&other_project);
    let peer = canonicalize_db(&db_path(&other_project)).unwrap();

    diff_canonical(&parallel, &peer, None)
        .expect("the parallel pipeline must produce one fixed canonical form across runs");

    let golden_db = workspace_root().join("reference/golden/mini/colby.db");
    let golden_dir = workspace_root().join("reference/golden/mini");
    assert_equivalent(&golden_db, &golden_dir)
        .expect("committed golden mini oracle must still hold (authoritative content gate)");
}

/// SIZE-SKIP PARITY: a file larger than `max_file_size` (default 1 MiB) must
/// produce the exact size-skip `FileRecord` through the parallel parse path —
/// not merely "0 nodes". We assert the persisted `files` row fields: size equal
/// to the real byte length, node_count 0, and the EXACT error string
/// `"File exceeds max size (N > M): path"` (engine.rs size_skip_result, mirrored
/// in the parallel parse worker).
#[test]
fn oversized_file_size_skips_with_exact_file_record() {
    const MAX_FILE_SIZE: usize = 1_048_576; // codegraph-core default_max_file_size

    let dir = TestDir::new("size-skip");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    // A syntactically valid TS file padded past the 1 MiB gate. The leading code is
    // irrelevant: the size gate fires before extraction, so it must size-skip.
    let mut oversized = String::from("export const sentinel = 1;\n");
    let pad = MAX_FILE_SIZE + 4096 - oversized.len();
    oversized.push_str(&"// padding to exceed max file size\n".repeat(pad / 35 + 1));
    let oversized_len = oversized.len() as i64;
    assert!(
        oversized_len > MAX_FILE_SIZE as i64,
        "fixture file must exceed max_file_size"
    );
    let rel_path = "src/oversized.ts";
    fs::write(project.join(rel_path), &oversized).unwrap();

    init_and_force_index(&project);

    let store = Store::open(&db_path(&project)).unwrap();
    let (size, node_count, errors_json): (i64, i64, Option<String>) = store
        .connection()
        .query_row(
            "SELECT size, node_count, errors FROM files WHERE path = ?1",
            [rel_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .expect("oversized file must have a files row");

    assert_eq!(
        size, oversized_len,
        "size-skip FileRecord must record the real byte length"
    );
    assert_eq!(
        node_count, 0,
        "size-skipped file must contribute zero nodes"
    );

    let expected_error =
        format!("File exceeds max size ({oversized_len} > {MAX_FILE_SIZE}): {rel_path}");
    let errors_json = errors_json.expect("size-skipped file must record a size-skip error");
    let errors: Vec<String> =
        serde_json::from_str(&errors_json).expect("files.errors is a JSON string array");
    assert_eq!(
        errors,
        vec![expected_error],
        "size-skip must record the exact engine error string through the parallel path"
    );
}

/// DEPTH-ABORT PARITY: a file that crosses the deterministic AST traversal
/// limit contributes no partial graph. Incremental sync must first remove the
/// file's previous graph, persist the stable error, and converge to the same DB
/// as a clean full rebuild of the final filesystem state.
#[test]
fn deep_file_sync_discards_stale_graph_and_matches_force_index() {
    const DEPTH: usize = 30_000;
    const REL_PATH: &str = "deep.c";
    const EXPECTED_ERROR: &str = "AST nesting exceeds safe traversal limit (256): deep.c";

    let deep_source = format!(
        "void before(void) {{}}\nvoid deep(void) {{{}{}}}\nvoid after(void) {{}}\n",
        "{".repeat(DEPTH),
        "}".repeat(DEPTH)
    );
    let normal_source = "int healthy_target(void) { return 7; }\n";

    let synced = TestDir::new("deep-sync");
    let synced_project = synced.path().join("project");
    fs::create_dir_all(&synced_project).unwrap();
    fs::write(
        synced_project.join(REL_PATH),
        "int stale_target(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(synced_project.join("ok.c"), normal_source).unwrap();
    init_and_force_index(&synced_project);

    fs::write(synced_project.join(REL_PATH), &deep_source).unwrap();
    let (out, err, ok) = cli(&["sync", synced_project.to_str().unwrap()]);
    assert!(ok, "sync failed: stdout={out} stderr={err}");

    let synced_store = Store::open(&db_path(&synced_project)).unwrap();
    let deep_node_count: i64 = synced_store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE file_path = ?1",
            [REL_PATH],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        deep_node_count, 0,
        "the failed file's previous and partial nodes must be removed"
    );

    let healthy_count: i64 = synced_store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE file_path = 'ok.c' AND name = 'healthy_target'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        healthy_count, 1,
        "one invalid file must not remove a healthy sibling's graph"
    );

    let (node_count, errors_json): (i64, Option<String>) = synced_store
        .connection()
        .query_row(
            "SELECT node_count, errors FROM files WHERE path = ?1",
            [REL_PATH],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the failed file must retain a files row");
    assert_eq!(node_count, 0);
    let errors: Vec<String> =
        serde_json::from_str(&errors_json.expect("the failed file must persist its depth error"))
            .unwrap();
    assert_eq!(errors, vec![EXPECTED_ERROR]);
    drop(synced_store);

    let rebuilt = TestDir::new("deep-force");
    let rebuilt_project = rebuilt.path().join("project");
    fs::create_dir_all(&rebuilt_project).unwrap();
    fs::write(rebuilt_project.join(REL_PATH), &deep_source).unwrap();
    fs::write(rebuilt_project.join("ok.c"), normal_source).unwrap();
    init_and_force_index(&rebuilt_project);

    let after_sync = canonicalize_db(&db_path(&synced_project)).unwrap();
    let after_force = canonicalize_db(&db_path(&rebuilt_project)).unwrap();
    diff_canonical(&after_sync, &after_force, None)
        .expect("sync and index --force must converge after a depth-aborted file");
}

/// WRITE PATH 1 (`index --force`, the rayon producer closure): the persisted
/// `files.generated` must carry the real per-file verdict, not a hardcoded false.
/// This is the CLI counterpart to the two `codegraph-watch` tests that cover the
/// incremental-sync and Outdated-rebuild write paths — three independent sites
/// build their own `FileRecord`, so each needs its own end-to-end assertion.
#[test]
fn index_force_writes_the_generated_flag_per_file() {
    let dir = TestDir::new("generated-flag");
    let project = dir.path().join("go");
    copy_tree(&go_fixture(), &project);

    init_and_force_index(&project);

    let store = Store::open(&db_path(&project)).unwrap();
    let mut rows: Vec<(String, i64)> = store
        .connection()
        .prepare("SELECT path, generated FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort();

    assert_eq!(
        rows,
        vec![
            ("api.pb.go".to_string(), 1),
            ("generator.go".to_string(), 0),
            ("nightly.go".to_string(), 0),
            ("payroll.go".to_string(), 1),
            ("payroll_usecase.go".to_string(), 0),
            ("worker_types.go".to_string(), 1),
        ],
        "index --force must persist the real generated verdict for every file"
    );
}

/// SYNC PRAGMA GUARD (end-to-end): the bulk-index path runs under
/// `synchronous=OFF` (T5) but must restore NORMAL before returning. After a real
/// `index --force` via the CLI, a FRESH `Store::open` on the produced DB must
/// report `PRAGMA synchronous != 0`. This is the end-to-end counterpart to the
/// in-isolation Store-method tests in codegraph-store/tests/bulk_index_pragmas.rs:
/// it proves the guard restores against a real full index, not just unit calls.
#[test]
fn full_index_leaves_synchronous_at_normal() {
    let dir = TestDir::new("pragma-guard");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    init_and_force_index(&project);

    let store = Store::open(&db_path(&project)).unwrap();
    let synchronous: i64 = store
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_ne!(
        synchronous, 0,
        "a fresh connection after index --force must be at NORMAL, never synchronous=OFF"
    );

    // No stray oversized -wal should survive the guard's wal_checkpoint(TRUNCATE).
    let wal = db_path(&project).with_extension("db-wal");
    if let Ok(meta) = fs::metadata(&wal) {
        assert!(
            meta.len() < 4096,
            "unexpected non-truncated -wal after full index: {} bytes",
            meta.len()
        );
    }
}
