//! Batch M — `full_sync_finalizer_publishes_current_last` public-surface Red.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md` lines 548-556 and test 8
//! (line 746) require that a destructive v2 rebuild becomes a READABLE `Current`
//! namespace only after every SQLite finalization step succeeded under the one
//! retained exclusive lease: pragma restore, final checkpoint + compaction,
//! extraction-version stamp, stamp checkpoint, and the final connection close.
//! Only then may the rebuild atomically publish `phase=current`.
//!
//! This file is the black-box half of that slice: it drives the shipped
//! `codegraph init` / `codegraph index --force` commands and then asks the
//! state-gated store layer (the accepted `Store::extraction_status` /
//! `Store::open_for_read` APIs from `f5c57f5`) whether the produced namespace is
//! a readable `Current`. Before the finalizer lands, `init` builds the DB but
//! publishes NO state slot at all, so classification is `Missing` and
//! `open_for_read` refuses — a behavioral failure, not a compile or setup one
//! (both assertions below are reached only after `init` succeeded and produced a
//! non-empty DB).
//!
//! The deterministic fault matrix over every finalization boundary lives in the
//! owning crate (`codegraph-store`, `src/rebuild.rs`), because the injection
//! seams are private by design.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{ExtractionStatus, IndexLease, StatePhase, Store, publish_index_state};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

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

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-batchm-finalizer-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
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

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn normalize(stream: &str) -> String {
    stream
        .lines()
        .filter(|line| !line.contains("logger initialized"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn final_error_body(stderr: &str) -> String {
    let normalized = normalize(stderr);
    let non_empty = normalized
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let error_lines = non_empty
        .iter()
        .filter(|line| line.starts_with("Error: "))
        .collect::<Vec<_>>();
    assert_eq!(
        error_lines.len(),
        1,
        "stderr must contain exactly one Error-prefixed line: {normalized:?}"
    );
    let final_line = non_empty
        .last()
        .expect("stderr must contain a final non-empty error line");
    assert_eq!(
        *final_line, *error_lines[0],
        "the unique Error-prefixed line must be final: {normalized:?}"
    );
    final_line
        .strip_prefix("Error: ")
        .expect("the final line was proven to have the Error prefix")
        .to_string()
}

fn run_in(registry_dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(bin())
        .args(args)
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn deadline() -> Instant {
    Instant::now()
        .checked_add(Duration::from_secs(10))
        .expect("finalizer test deadline")
}

/// Assert the namespace produced by a finished rebuild is a readable `Current`:
/// the state slots classify `Current`, the tombstone is absent, the explicit
/// checkpoint/compaction/close pipeline satisfies the sidecar-free Current
/// artifact contract, and a state-gated read open serves rows.
fn assert_readable_current(paths: &IndexPaths, context: &str) {
    let status = Store::extraction_status(paths);
    assert_eq!(
        status,
        ExtractionStatus::Current,
        "{context}: the finished rebuild must publish phase=current as its LAST step \
         (plan lines 548-556); observed {status:?}"
    );
    assert!(
        !paths.tombstone().exists(),
        "{context}: a successful explicit init must leave no tombstone at {}",
        paths.tombstone().display()
    );
    let db = paths.current_db();
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db.display()));
        assert!(
            !sidecar.exists(),
            "{context}: final checkpoint/compaction and connection close must satisfy \
             the sidecar-free Current artifact contract before publication; found {}",
            sidecar.display()
        );
    }
    let store = Store::open_for_read(paths, deadline(), || false).unwrap_or_else(|error| {
        panic!("{context}: a finalized namespace must be readable through the state gate: {error}")
    });
    let counts = store
        .counts()
        .unwrap_or_else(|error| panic!("{context}: counts on a finalized namespace: {error}"));
    assert!(
        counts.node_count > 0,
        "{context}: the finalized readable namespace must serve the built graph, got {} nodes",
        counts.node_count
    );
}

/// Stage the durable state left by an interrupted destructive rebuild. The
/// permanent lock and `phase=building` slot are authentic; the final SQLite DB
/// is independently present or absent to cover both user-reachable crash
/// windows (before or after the fresh writer opens).
fn stage_building_with_lease(paths: &IndexPaths, db_present: bool) -> IndexLease {
    let lease = IndexLease::create_exclusive(paths, deadline(), || false)
        .expect("create the interrupted-rebuild namespace");
    publish_index_state(paths, &lease, StatePhase::Building).expect("publish phase=building");
    if db_present {
        let store = Store::open(&paths.current_db()).expect("stage the partial rebuild DB");
        store
            .set_project_metadata("partial-rebuild", "true")
            .expect("write partial rebuild evidence");
        drop(store);
    }
    assert_eq!(
        Store::extraction_status(paths),
        ExtractionStatus::Building {
            built: codegraph_store::CURRENT_EXTRACTION_VERSION,
        }
    );
    lease
}

fn stage_interrupted_building(paths: &IndexPaths, db_present: bool) {
    drop(stage_building_with_lease(paths, db_present));
}

fn building_error(project: &Path) -> String {
    format!(
        "CodeGraph index build was interrupted in {}; reads remain blocked to avoid false empty results. Run `codegraph index --force {}` to rebuild it (or `codegraph init {}`).",
        project.display(),
        project.display(),
        project.display()
    )
}

fn slot_bytes(paths: &IndexPaths) -> [Option<Vec<u8>>; 2] {
    paths.state_slots().map(|slot| match std::fs::read(&slot) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read state slot {}: {error}", slot.display()),
    })
}

#[test]
fn building_state_recovery_status_guides_only_stale_building() {
    let dir = TestDir::new("status-stale-building");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    stage_interrupted_building(&paths, false);
    let project_arg = project.to_str().expect("UTF-8 project path");

    let human = run_in(dir.path(), &["status", project_arg]);
    assert!(human.ok, "human status must succeed: {}", human.stderr);
    assert_eq!(normalize(&human.stderr), "");
    assert_eq!(
        human.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   building (extraction version {})\nDaemon:  stopped\nIndex is not readable while the build is incomplete.\nRecovery: run `codegraph index --force {}` to rebuild the interrupted index; `codegraph init {}` is also supported.\n",
            project.display(),
            paths.current_db().display(),
            codegraph_store::CURRENT_EXTRACTION_VERSION,
            project.display(),
            project.display()
        )
    );

    let json_run = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(json_run.ok, "JSON status must succeed: {}", json_run.stderr);
    assert_eq!(normalize(&json_run.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_run.stdout.trim()).expect("status emits JSON");
    assert_eq!(value["initialized"], false);
    assert_eq!(value["extractionStatus"], "building");
    assert_eq!(
        value["recoveryCommand"],
        format!("codegraph index --force {}", project.display())
    );
}

#[test]
fn building_state_recovery_status_never_guides_a_live_builder() {
    let dir = TestDir::new("status-live-building");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    let lease = stage_building_with_lease(&paths, false);

    let status = run_in(
        dir.path(),
        &[
            "status",
            "--json",
            project.to_str().expect("UTF-8 project path"),
        ],
    );
    assert!(
        status.ok,
        "live-building status must succeed: {}",
        status.stderr
    );
    assert_eq!(normalize(&status.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(status.stdout.trim()).expect("status emits JSON");
    assert_eq!(value["initialized"], false);
    assert_eq!(value["rebuilding"], true);
    assert_eq!(value["extractionStatus"], serde_json::Value::Null);
    assert!(value.get("recoveryCommand").is_none());
    drop(lease);
}

#[test]
fn building_state_recovery_unlock_clears_only_stale_locks_and_keeps_reads_blocked() {
    let dir = TestDir::new("unlock-stale-locks");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    stage_interrupted_building(&paths, false);
    let states_before = slot_bytes(&paths);
    std::fs::write(paths.daemon_pid(), b"stale pid record").expect("write stale daemon pid");
    let transient_lock = paths.current_root().join("codegraph.lock");
    std::fs::write(&transient_lock, b"stale lock").expect("write stale transient lock");
    let project_arg = project.to_str().expect("UTF-8 project path");

    let unlock = run_in(dir.path(), &["unlock", project_arg]);
    assert!(unlock.ok, "unlock must succeed: {}", unlock.stderr);
    assert_eq!(normalize(&unlock.stderr), "");
    assert_eq!(
        unlock.stdout,
        format!(
            "Removed lock file. You can now run indexing again.\nIndex state remains building; no rollback was performed. Run `codegraph index --force {}` to rebuild it (or `codegraph init {}`).\n",
            project.display(),
            project.display()
        )
    );
    assert!(!paths.daemon_pid().exists());
    assert!(!transient_lock.exists());
    assert!(paths.permanent_lock().is_file());
    assert!(!paths.current_db().exists());
    assert_eq!(slot_bytes(&paths), states_before);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Building {
            built: codegraph_store::CURRENT_EXTRACTION_VERSION,
        }
    );

    let query = run_in(
        dir.path(),
        &["query", "Counter", "-p", project_arg, "--strict"],
    );
    assert!(!query.ok, "unlock must not make Building readable");
    assert_eq!(final_error_body(&query.stderr), building_error(&project));
}

#[test]
fn building_state_recovery_unlock_without_stale_locks_still_guides_recovery() {
    let dir = TestDir::new("unlock-no-locks");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    stage_interrupted_building(&paths, false);
    let states_before = slot_bytes(&paths);

    let unlock = run_in(
        dir.path(),
        &["unlock", project.to_str().expect("UTF-8 project path")],
    );
    assert!(unlock.ok, "unlock must succeed: {}", unlock.stderr);
    assert_eq!(normalize(&unlock.stderr), "");
    assert_eq!(
        unlock.stdout,
        format!(
            "No lock file found - nothing to do\nIndex state remains building; no rollback was performed. Run `codegraph index --force {}` to rebuild it (or `codegraph init {}`).\n",
            project.display(),
            project.display()
        )
    );
    assert!(paths.permanent_lock().is_file());
    assert!(!paths.current_db().exists());
    assert_eq!(slot_bytes(&paths), states_before);
}

#[test]
fn building_state_recovery_unlock_never_competes_with_a_live_builder() {
    let dir = TestDir::new("unlock-live-building");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    let lease = stage_building_with_lease(&paths, false);
    let states_before = slot_bytes(&paths);

    let unlock = run_in(
        dir.path(),
        &["unlock", project.to_str().expect("UTF-8 project path")],
    );
    assert!(unlock.ok, "unlock must succeed: {}", unlock.stderr);
    assert_eq!(normalize(&unlock.stderr), "");
    assert_eq!(
        unlock.stdout,
        "No lock file found - nothing to do\nIndex build is still running; no recovery command was issued.\n"
    );
    assert!(paths.permanent_lock().is_file());
    assert!(!paths.current_db().exists());
    assert_eq!(slot_bytes(&paths), states_before);
    drop(lease);
}

/// Plan test 8 (line 746) public boundary: only a completely successful rebuild
/// becomes readable `Current`, and that holds for the initial explicit `init` as
/// well as for an ordinary destructive `index --force` over the finished index.
#[test]
fn full_rebuild_publishes_readable_current_last_via_cli() {
    let dir = TestDir::new("current-last");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let run = run_in(dir.path(), &["init", p]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed before the behavioral assertion \
         (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    assert!(
        std::fs::metadata(paths.current_db())
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        "setup: `init` must produce a non-empty DB at {}",
        paths.current_db().display()
    );

    assert_readable_current(&paths, "explicit init");

    let reindex = run_in(dir.path(), &["index", "--force", p]);
    assert!(
        reindex.ok,
        "setup: `codegraph index --force` must succeed: {} {}",
        reindex.stdout, reindex.stderr
    );
    assert_readable_current(&paths, "index --force");
}

/// A raw partial DB is not proof of a healthy initialized index. Explicit init
/// is the recovery surface for either interrupted-Building crash window.
#[test]
fn explicit_init_recovers_interrupted_building_with_db_present_or_absent() {
    for db_present in [false, true] {
        let dir = TestDir::new(if db_present {
            "init-building-db-present"
        } else {
            "init-building-db-absent"
        });
        let project = dir.path().join("mini");
        copy_tree(&mini_fixture(), &project);
        let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
        stage_interrupted_building(&paths, db_present);

        let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
        assert!(
            run.ok,
            "explicit init must recover Building (db_present={db_present}): stdout={}, stderr={}",
            run.stdout, run.stderr
        );
        assert!(
            !run.stdout.contains("Already initialized"),
            "a partial DB must not be mistaken for healthy Current: {}",
            run.stdout
        );
        assert_readable_current(&paths, "explicit init retry after Building");
    }
}

/// Ordinary index may retry an authenticated Building rebuild, whether the
/// previous process crashed before or after creating the final DB artifact.
#[test]
fn index_recovers_interrupted_building_with_db_present_or_absent() {
    for db_present in [false, true] {
        let dir = TestDir::new(if db_present {
            "index-building-db-present"
        } else {
            "index-building-db-absent"
        });
        let project = dir.path().join("mini");
        copy_tree(&mini_fixture(), &project);
        let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
        stage_interrupted_building(&paths, db_present);

        let run = run_in(dir.path(), &["index", "--force", project.to_str().unwrap()]);
        assert!(
            run.ok,
            "index must retry Building (db_present={db_present}): stdout={}, stderr={}",
            run.stdout, run.stderr
        );
        assert_readable_current(&paths, "index retry after Building");
    }
}

/// If the prior explicit init published Current but failed at tombstone removal,
/// the namespace remains deliberately unreadable. A repeated explicit init is
/// the recovery surface and must not misreport it as already initialized.
#[test]
fn explicit_init_recovers_current_with_tombstone_residue() {
    let dir = TestDir::new("init-current-tombstone");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let initial = run_in(dir.path(), &["init", p]);
    assert!(
        initial.ok,
        "initial init must succeed: {} {}",
        initial.stdout, initial.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve default index paths");
    std::fs::write(paths.tombstone(), b"remove failed")
        .expect("stage Current+tombstone finalizer residue");
    assert!(
        Store::open_for_read(&paths, deadline(), || false).is_err(),
        "Current+tombstone must not be readable"
    );

    let retry = run_in(dir.path(), &["init", p]);
    assert!(
        retry.ok,
        "explicit init must recover Current+tombstone: stdout={}, stderr={}",
        retry.stdout, retry.stderr
    );
    assert!(
        !retry.stdout.contains("Already initialized"),
        "Current+tombstone is not a healthy Current namespace: {}",
        retry.stdout
    );
    assert_readable_current(
        &paths,
        "explicit init retry after tombstone removal failure",
    );
}
