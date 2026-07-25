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

fn assert_legacy_bytes(legacy_file: &Path, expected: &[u8]) {
    assert_eq!(
        std::fs::read(legacy_file).expect("read untouched legacy proof"),
        expected,
        "v2 lifecycle operations must not mutate the legacy namespace"
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
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
    let legacy = project.join(".codegraph");
    std::fs::create_dir(&legacy).expect("create independent legacy namespace");
    let legacy_file = legacy.join("legacy.bin");
    let legacy_bytes = b"legacy bytes must remain unchanged";
    std::fs::write(&legacy_file, legacy_bytes).expect("write legacy proof bytes");

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
        "uninit --force must durably publish phase=uninitialized before cleanup; deleting the whole v2 root loses the authenticated recovery state"
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
        "successful uninit cleanup must remove the v2 database"
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
    assert_legacy_bytes(&legacy_file, legacy_bytes);

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
    assert_eq!(status_json["legacyIndexPresent"], true);

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
        assert_legacy_bytes(&legacy_file, legacy_bytes);
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
    assert_legacy_bytes(&legacy_file, legacy_bytes);

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
    assert_legacy_bytes(&legacy_file, legacy_bytes);
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
fn absolute_configured_root_requires_explicit_project_for_nested_uninit() {
    let dir = TestDir::new("absolute-configured-root");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let nested = project.join("src/nested");
    std::fs::create_dir_all(&nested).expect("create nested invocation directory");
    let configured = dir.path().join("shared-index");
    let project_arg = project.to_str().expect("UTF-8 project path");

    let init = run_in_with_config(dir.path(), &["init", project_arg], Some(&configured));
    assert!(
        init.ok,
        "configured init failed: {} {}",
        init.stdout, init.stderr
    );
    let paths =
        IndexPaths::resolve(&project, configured.to_str()).expect("resolve configured root");
    let before = namespace_snapshot(paths.current_root());

    let implicit = run_in_with_config(&nested, &["uninit", "--force"], Some(&configured));
    assert!(!implicit.ok, "nested implicit uninit must fail");
    assert!(
        implicit.stderr.contains("pass the project root explicitly"),
        "stable remedy missing: {}",
        implicit.stderr
    );
    assert_namespace_unchanged(
        &before,
        &namespace_snapshot(paths.current_root()),
        "nested implicit absolute-root uninit refusal",
    );

    let explicit = run_in_with_config(
        &nested,
        &["uninit", "--force", project_arg],
        Some(&configured),
    );
    assert!(
        explicit.ok,
        "explicit project-root uninit must succeed: {} {}",
        explicit.stdout, explicit.stderr
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Uninitialized
    );
}

fn stage_lifecycle_state(paths: &IndexPaths, status: &str) {
    let lease = IndexLease::create_exclusive(paths, deadline(), || false)
        .expect("create lifecycle namespace");
    match status {
        "building" => {
            publish_index_state(paths, &lease, StatePhase::Building).expect("publish Building");
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
                    std::fs::write(&paths.state_slots()[0], b"not-json")
                        .expect("write corrupt slot");
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
    let configured = Path::new("cache/index");
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
    let nested = project.join("a/b");
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
