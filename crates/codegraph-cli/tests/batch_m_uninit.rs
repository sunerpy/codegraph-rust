//! Batch M — interrupted `uninit --force` lifecycle acceptance.
//!
//! Revision 14 requires public uninit to publish an authoritative
//! `phase=uninitialized` slot before deleting any v2 residue, preserve the
//! permanent lock and both fixed state slots, and leave only explicit `init` or
//! another `uninit --force` continuation authorized. The store-owned fault
//! matrix for every private mutation boundary lives in `codegraph-store`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, IndexLease, StatePhase,
    Store, checksum_hex, classify, publish_index_state,
};

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

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-batchm-uninit-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create uninit test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create fixture destination");
    for entry in std::fs::read_dir(src).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy fixture file");
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
    run_in_with_config(registry_dir, args, None)
}

fn run_in_with_config(registry_dir: &Path, args: &[&str], codegraph_dir: Option<&Path>) -> Run {
    let mut command = Command::new(bin());
    command
        .current_dir(registry_dir)
        .args(args)
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1");
    if let Some(configured) = codegraph_dir {
        command.env("CODEGRAPH_DIR", configured);
    } else {
        command.env_remove("CODEGRAPH_DIR");
    }
    let output = command.output().expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn db_sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut native = db.as_os_str().to_os_string();
    native.push(suffix);
    PathBuf::from(native)
}

#[derive(Debug, PartialEq, Eq)]
enum NamespaceEntry {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn namespace_snapshot(root: &Path) -> BTreeMap<PathBuf, NamespaceEntry> {
    fn walk(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, NamespaceEntry>) {
        let mut children = std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("snapshot read_dir {}: {error}", directory.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| {
                        panic!("snapshot entry in {}: {error}", directory.display())
                    })
                    .path()
            })
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|error| panic!("snapshot strip {}: {error}", path.display()))
                .to_path_buf();
            let metadata = std::fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("snapshot metadata {}: {error}", path.display()));
            let ty = metadata.file_type();
            let entry =
                if ty.is_dir() {
                    NamespaceEntry::Directory
                } else if ty.is_file() {
                    NamespaceEntry::File(std::fs::read(&path).unwrap_or_else(|error| {
                        panic!("snapshot read {}: {error}", path.display())
                    }))
                } else if ty.is_symlink() {
                    NamespaceEntry::Symlink(std::fs::read_link(&path).unwrap_or_else(|error| {
                        panic!("snapshot read_link {}: {error}", path.display())
                    }))
                } else {
                    panic!("snapshot unsupported entry kind: {}", path.display());
                };
            assert!(
                out.insert(relative, entry).is_none(),
                "duplicate snapshot path"
            );
            if ty.is_dir() {
                walk(root, &path, out);
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn assert_namespace_unchanged(
    before: &BTreeMap<PathBuf, NamespaceEntry>,
    after: &BTreeMap<PathBuf, NamespaceEntry>,
    label: &str,
) {
    let mut all_paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    all_paths.sort();
    all_paths.dedup();
    let changed = all_paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "{label} changed namespace entries: {changed:?}"
    );
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn assert_unrelated_bytes(unrelated_file: &Path, expected: &[u8]) {
    assert_eq!(
        std::fs::read(unrelated_file).expect("read untouched unrelated proof"),
        expected,
        "index lifecycle operations must not mutate unrelated project data"
    );
}

/// Public behavioral Red for acceptance item 11: successful uninit must leave a
/// recoverable authenticated lifecycle namespace, not erase its state root.
#[test]
fn interrupted_uninit_state_slot_is_recoverable_not_corrupt() {
    let dir = TestDir::new("public-red");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let project_arg = project.to_str().expect("UTF-8 test project path");

    let init = run_in(dir.path(), &["init", project_arg]);
    assert!(
        init.ok,
        "setup: init must succeed before the lifecycle assertion: stdout={}, stderr={}",
        init.stdout, init.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve index paths");
    let unrelated = project.join("unrelated-cache");
    std::fs::create_dir(&unrelated).expect("create unrelated project directory");
    let unrelated_file = unrelated.join("keep.bin");
    let unrelated_bytes = b"unrelated bytes must remain unchanged";
    std::fs::write(&unrelated_file, unrelated_bytes).expect("write unrelated proof bytes");

    for (path, bytes) in [
        (
            db_sidecar(&paths.current_db(), "-wal"),
            b"wal residue".as_slice(),
        ),
        (
            db_sidecar(&paths.current_db(), "-shm"),
            b"shm residue".as_slice(),
        ),
        (paths.config_toml(), b"config residue".as_slice()),
        (paths.extension_config(), b"extension residue".as_slice()),
        (paths.daemon_pid(), b"pid residue".as_slice()),
        (paths.daemon_log(), b"log residue".as_slice()),
        (paths.daemon_socket(), b"socket residue".as_slice()),
    ] {
        std::fs::write(path, bytes).expect("stage removable v2 residue");
    }

    let uninit = run_in(dir.path(), &["uninit", "--force", project_arg]);
    assert!(
        uninit.ok,
        "uninit --force must succeed: stdout={}, stderr={}",
        uninit.stdout, uninit.stderr
    );

    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Uninitialized,
        "uninit --force must durably publish phase=uninitialized before cleanup; deleting the whole index root loses the authenticated recovery state"
    );
    assert!(
        paths.permanent_lock().is_file(),
        "uninit must preserve the permanent lock"
    );
    assert!(
        paths.tombstone().is_file(),
        "uninit must ensure the tombstone"
    );
    assert!(
        paths.state_slots().iter().all(|slot| slot.is_file()),
        "uninit must preserve both fixed state slots"
    );
    assert!(
        !paths.current_db().exists(),
        "successful uninit cleanup must remove the database"
    );
    for path in [
        db_sidecar(&paths.current_db(), "-wal"),
        db_sidecar(&paths.current_db(), "-shm"),
        paths.config_toml(),
        paths.extension_config(),
        paths.daemon_pid(),
        paths.daemon_log(),
        paths.daemon_socket(),
    ] {
        assert!(
            !path.exists(),
            "successful uninit must remove v2 lifecycle residue: {}",
            path.display()
        );
    }
    assert_unrelated_bytes(&unrelated_file, unrelated_bytes);

    let status = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(
        status.ok,
        "status must report interrupted uninit: stdout={}, stderr={}",
        status.stdout, status.stderr
    );
    let status_json: serde_json::Value =
        serde_json::from_str(status.stdout.trim()).expect("status emits JSON");
    assert_eq!(status_json["initialized"], false);
    assert_eq!(status_json["extractionStatus"], "uninitialized");
    assert_eq!(status_json["legacyIndexPresent"], false);
    assert_eq!(status_json["legacyIndexPaths"], serde_json::json!([]));

    for args in [
        vec!["sync", project_arg],
        vec!["index", "--force", project_arg],
        vec!["index", project_arg],
        vec!["query", "Counter", "--path", project_arg],
    ] {
        let before_rejection = namespace_snapshot(paths.current_root());
        let rejected = run_in(dir.path(), &args);
        assert!(
            !rejected.ok,
            "only init or repeated uninit may proceed after interrupted uninit; args={args:?}, stdout={}, stderr={}",
            rejected.stdout, rejected.stderr
        );
        assert_namespace_unchanged(
            &before_rejection,
            &namespace_snapshot(paths.current_root()),
            &format!("rejected command {args:?}"),
        );
        assert_unrelated_bytes(&unrelated_file, unrelated_bytes);
    }

    let first_sequence = classify(&paths)
        .authoritative()
        .expect("uninit has authoritative slot")
        .record
        .sequence;
    let continuation = run_in(dir.path(), &["uninit", "--force", project_arg]);
    assert!(
        continuation.ok,
        "repeated uninit must continue cleanup: stdout={}, stderr={}",
        continuation.stdout, continuation.stderr
    );
    let continued = classify(&paths);
    assert_eq!(continued.status(), &ExtractionStatus::Uninitialized);
    assert_eq!(
        continued
            .authoritative()
            .expect("continued uninit has authoritative slot")
            .record
            .sequence,
        first_sequence + 1,
        "continued uninit must publish a monotonic Uninitialized sequence"
    );
    assert_unrelated_bytes(&unrelated_file, unrelated_bytes);

    let recovery = run_in(dir.path(), &["init", project_arg]);
    assert!(
        recovery.ok,
        "explicit init must recover interrupted uninit: stdout={}, stderr={}",
        recovery.stdout, recovery.stderr
    );
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    assert!(
        paths.current_db().is_file(),
        "init recovery rebuilds the DB"
    );
    assert!(
        !paths.tombstone().exists(),
        "only successful explicit init recovery removes the tombstone"
    );
    assert_unrelated_bytes(&unrelated_file, unrelated_bytes);
}

#[test]
fn namespace_snapshot_detects_equal_length_byte_mutation() {
    let dir = TestDir::new("snapshot-self-test");
    let root = dir.path().join("namespace");
    std::fs::create_dir(&root).expect("create snapshot root");
    let file = root.join("state.json");
    std::fs::write(&file, b"AAAA").expect("write original bytes");
    let before = namespace_snapshot(&root);
    std::fs::write(&file, b"BBBB").expect("write equal-length replacement");
    let after = namespace_snapshot(&root);
    assert!(
        std::panic::catch_unwind(|| {
            assert_namespace_unchanged(&before, &after, "snapshot self-test")
        })
        .is_err(),
        "namespace oracle must detect equal-length byte mutation"
    );
}

#[test]
fn absolute_configured_root_is_rejected_without_creating_an_index() {
    let dir = TestDir::new("absolute-configured-root");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let configured = dir.path().join("shared-index");
    let project_arg = project.to_str().expect("UTF-8 project path");

    let init = run_in_with_config(dir.path(), &["init", project_arg], Some(&configured));
    assert!(
        !init.ok,
        "absolute configured root must be rejected: {} {}",
        init.stdout, init.stderr
    );
    assert!(
        init.stderr
            .contains("must be one non-empty project-local directory name"),
        "stable rejection missing: {}",
        init.stderr
    );
    assert!(
        !configured.exists(),
        "a rejected absolute root must not be created"
    );
    assert!(!project.join(".codegraph").exists());
}

fn stage_lifecycle_state(paths: &IndexPaths, status: &str) {
    let lease = IndexLease::create_exclusive(paths, deadline(), || false)
        .expect("create lifecycle namespace");
    match status {
        "building" => {
            publish_index_state(paths, &lease, StatePhase::Building).expect("publish Building");
        }
        "uninitialized" => {
            let phase = StatePhase::Uninitialized.as_wire();
            let checksum = checksum_hex(
                0,
                CURRENT_STORAGE_PROTOCOL,
                CURRENT_EXTRACTION_VERSION,
                phase,
                paths.project_identity(),
            );
            let body = serde_json::json!({
                "sequence": 0,
                "storageProtocol": CURRENT_STORAGE_PROTOCOL,
                "extractionVersion": CURRENT_EXTRACTION_VERSION,
                "phase": phase,
                "projectIdentity": paths.project_identity(),
                "checksum": checksum,
            });
            std::fs::write(
                &paths.state_slots()[0],
                serde_json::to_vec(&body).expect("serialize Uninitialized slot"),
            )
            .expect("write Uninitialized slot");
        }
        "current-no-db" => {
            publish_index_state(paths, &lease, StatePhase::Building)
                .expect("publish Building before Current");
            publish_index_state(paths, &lease, StatePhase::Current).expect("publish Current");
        }
        "outdated" | "future" | "corrupt" => {
            let (storage_protocol, extraction_version, phase) = match status {
                "outdated" => (
                    CURRENT_STORAGE_PROTOCOL,
                    CURRENT_EXTRACTION_VERSION - 1,
                    "current",
                ),
                "future" => (
                    CURRENT_STORAGE_PROTOCOL + 1,
                    CURRENT_EXTRACTION_VERSION + 1,
                    "current",
                ),
                "corrupt" => {
                    for (slot, phase) in
                        paths.state_slots().into_iter().zip(["current", "building"])
                    {
                        let checksum = checksum_hex(
                            7,
                            CURRENT_STORAGE_PROTOCOL,
                            CURRENT_EXTRACTION_VERSION,
                            phase,
                            paths.project_identity(),
                        );
                        let body = serde_json::json!({
                            "sequence": 7,
                            "storageProtocol": CURRENT_STORAGE_PROTOCOL,
                            "extractionVersion": CURRENT_EXTRACTION_VERSION,
                            "phase": phase,
                            "projectIdentity": paths.project_identity(),
                            "checksum": checksum,
                        });
                        std::fs::write(
                            slot,
                            serde_json::to_vec(&body).expect("serialize equal-sequence slot"),
                        )
                        .expect("write equal-sequence slot");
                    }
                    drop(lease);
                    return;
                }
                _ => unreachable!(),
            };
            let checksum = checksum_hex(
                0,
                storage_protocol,
                extraction_version,
                phase,
                paths.project_identity(),
            );
            let body = serde_json::json!({
                "sequence": 0,
                "storageProtocol": storage_protocol,
                "extractionVersion": extraction_version,
                "phase": phase,
                "projectIdentity": paths.project_identity(),
                "checksum": checksum,
            });
            std::fs::write(
                &paths.state_slots()[0],
                serde_json::to_vec(&body).expect("serialize lifecycle slot"),
            )
            .expect("write lifecycle slot");
        }
        _ => unreachable!(),
    }
    drop(lease);
}

fn assert_query_error(dir: &TestDir, project: &Path, expected: &str) {
    let project_arg = project.to_str().expect("UTF-8 test project path");
    let run = run_in(
        dir.path(),
        &["query", "Counter", "-p", project_arg, "--strict"],
    );
    assert!(
        !run.ok,
        "query must fail closed: stdout={}, stderr={}",
        run.stdout, run.stderr
    );
    assert_eq!(final_error_body(&run.stderr), expected);
}

#[test]
fn building_state_recovery_reports_exact_per_state_query_diagnostics() {
    for status in ["building", "uninitialized", "outdated", "future", "corrupt"] {
        let dir = TestDir::new(status);
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("create lifecycle project");
        let paths = IndexPaths::resolve(&project, None).expect("resolve lifecycle paths");
        stage_lifecycle_state(&paths, status);
        let project_display = project.display();
        let expected = match status {
            "building" => format!(
                "CodeGraph index build was interrupted in {project_display}; reads remain blocked to avoid false empty results. Run `codegraph index --force {project_display}` to rebuild it (or `codegraph init {project_display}`)."
            ),
            "uninitialized" => format!(
                "CodeGraph index removal was interrupted in {project_display}; run `codegraph init {project_display}` to rebuild it"
            ),
            "outdated" => format!(
                "CodeGraph index in {project_display} is outdated (built with extraction version {}); run `codegraph index --force {project_display}` to rebuild it",
                CURRENT_EXTRACTION_VERSION - 1
            ),
            "future" => format!(
                "CodeGraph index in {project_display} was built by a newer CodeGraph version (extraction version {}); upgrade CodeGraph before reading it",
                CURRENT_EXTRACTION_VERSION + 1
            ),
            "corrupt" => format!(
                "CodeGraph index state in {project_display} is corrupt: both index state slots are valid at sequence 7 with differing payloads; run `codegraph status {project_display}` for details; manual recovery is required"
            ),
            _ => unreachable!(),
        };
        assert_query_error(&dir, &project, &expected);
    }

    let dir = TestDir::new("fresh-missing");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("create fresh project");
    let expected = format!(
        "CodeGraph not initialized in {}; run `codegraph init {}` to create or replace the index",
        project.display(),
        project.display()
    );
    assert_query_error(&dir, &project, &expected);
}

#[test]
fn building_state_recovery_reports_missing_state_for_every_database_artifact() {
    for suffix in [None, Some("-wal"), Some("-shm")] {
        let label = suffix.unwrap_or("database").trim_start_matches('-');
        let dir = TestDir::new(&format!("missing-state-{label}"));
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let paths = IndexPaths::resolve(&project, None).expect("resolve index paths");
        std::fs::create_dir_all(paths.current_root()).expect("create index root");
        let artifact = suffix.map_or_else(
            || paths.current_db(),
            |value| db_sidecar(&paths.current_db(), value),
        );
        std::fs::write(&artifact, b"stale database artifact").expect("write database artifact");
        let expected = format!(
            "index database has no state slots and may have been created by an older version or another tool; run `codegraph init {}` to replace it",
            project.display()
        );
        assert_query_error(&dir, &project, &expected);
    }
}

#[test]
fn building_state_recovery_current_without_database_names_the_working_manual_remedy() {
    let dir = TestDir::new("current-no-db");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve index paths");
    stage_lifecycle_state(&paths, "current-no-db");
    let diagnostic_paths =
        IndexPaths::resolve(&project, None).expect("re-resolve published index root");
    let expected = format!(
        "CodeGraph index state is current in {}, but {} is missing; no CodeGraph CLI command can recover this externally damaged namespace. After confirming no CodeGraph process is using it, run `rm -rf -- \"{}\" && codegraph init \"{}\"`.",
        project.display(),
        diagnostic_paths.current_db().display(),
        diagnostic_paths.current_root().display(),
        project.display()
    );
    assert_query_error(&dir, &project, &expected);

    std::fs::remove_dir_all(diagnostic_paths.current_root())
        .expect("remove externally damaged index root");
    let recovery = run_in(
        dir.path(),
        &["init", project.to_str().expect("UTF-8 test project path")],
    );
    assert!(
        recovery.ok,
        "the exact manual remedy named by the diagnostic must recover: stdout={}, stderr={}",
        recovery.stdout, recovery.stderr
    );
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    assert!(paths.current_db().is_file());
}

#[test]
fn building_state_recovery_current_database_remains_readable() {
    let dir = TestDir::new("current-readable");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let project_arg = project.to_str().expect("UTF-8 test project path");
    let init = run_in(dir.path(), &["init", project_arg]);
    assert!(init.ok, "init must establish Current: {}", init.stderr);

    let query = run_in(
        dir.path(),
        &["query", "Counter", "-p", project_arg, "--strict"],
    );
    assert!(
        query.ok,
        "Current plus database must remain readable: stdout={}, stderr={}",
        query.stdout, query.stderr
    );
}

#[test]
fn nested_status_discovers_authenticated_non_current_parent_states() {
    for expected in ["building", "outdated", "future", "corrupt"] {
        let dir = TestDir::new(expected);
        let project = dir.path().join("project");
        let nested = project.join("a/b");
        std::fs::create_dir_all(&nested).expect("create nested project directory");
        let paths = IndexPaths::resolve(&project, None).expect("resolve lifecycle paths");
        stage_lifecycle_state(&paths, expected);

        let status = run_in(&nested, &["status", "--json"]);
        assert!(
            status.ok,
            "nested {expected} status failed: {} {}",
            status.stdout, status.stderr
        );
        let value: serde_json::Value =
            serde_json::from_str(status.stdout.trim()).expect("parse status JSON");
        assert_eq!(value["projectPath"], project.to_string_lossy().as_ref());
        assert_eq!(value["extractionStatus"], expected);
        assert_eq!(value["initialized"], false);
    }
}

#[test]
fn nested_status_discovers_interrupted_uninit_with_relative_configured_root() {
    let dir = TestDir::new("nested-relative-uninit");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let nested = project.join("src/nested");
    std::fs::create_dir_all(&nested).expect("create nested invocation directory");
    let configured = Path::new("cache");
    let project_arg = project.to_str().expect("UTF-8 project path");
    let init = run_in_with_config(dir.path(), &["init", project_arg], Some(configured));
    assert!(init.ok, "relative configured init failed: {}", init.stderr);
    let uninit = run_in_with_config(
        dir.path(),
        &["uninit", "--force", project_arg],
        Some(configured),
    );
    assert!(
        uninit.ok,
        "relative configured uninit failed: {}",
        uninit.stderr
    );

    let status = run_in_with_config(&nested, &["status", "--json"], Some(configured));
    assert!(
        status.ok,
        "nested Uninitialized status failed: {}",
        status.stderr
    );
    let value: serde_json::Value =
        serde_json::from_str(status.stdout.trim()).expect("parse status JSON");
    assert_eq!(value["projectPath"], project.to_string_lossy().as_ref());
    assert_eq!(value["extractionStatus"], "uninitialized");
    assert_eq!(value["initialized"], false);
}

#[test]
fn lock_only_parent_is_not_a_status_discovery_marker() {
    let dir = TestDir::new("lock-only");
    let project = dir.path().join("project");
    // NOT `join("a/b")`: `Path::join` keeps an embedded `/` verbatim, so on
    // Windows that literal yields a MIXED `…\project\a/b`, while the CLI re-emits
    // this path through `normalize_lexical` as `…\project\a\b`. The assertion
    // below compares the reported `projectPath` to this value, so it must be
    // built in the same native-separator domain the CLI prints.
    let nested = project.join("a").join("b");
    std::fs::create_dir_all(&nested).expect("create nested project directory");
    let paths = IndexPaths::resolve(&project, None).expect("resolve lifecycle paths");
    std::fs::create_dir(paths.current_root()).expect("create current root");
    std::fs::write(paths.permanent_lock(), b"").expect("write lock-only fixture");

    let status = run_in(&nested, &["status", "--json"]);
    assert!(status.ok, "lock-only status failed: {}", status.stderr);
    let value: serde_json::Value =
        serde_json::from_str(status.stdout.trim()).expect("parse status JSON");
    assert_eq!(value["projectPath"], nested.to_string_lossy().as_ref());
    assert_eq!(value["extractionStatus"], "missing");
}
