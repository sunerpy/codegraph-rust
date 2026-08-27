//! Batch M — interrupted `uninit --force` lifecycle acceptance.
//!
//! Revision 14 requires public uninit to publish an authoritative
//! `phase=uninitialized` slot before deleting any v2 residue, preserve the
//! permanent lock and both fixed state slots, and leave only explicit `init` or
//! another `uninit --force` continuation authorized. The store-owned fault
//! matrix for every private mutation boundary lives in `codegraph-store`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, CorruptReason, ExtractionStatus,
    IndexLease, SlotOutcome, StatePhase, Store, checksum_hex, classify, publish_index_state,
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
        .map(|line| {
            let is_summary = line.contains(" nodes, ") && line.contains(" edges in ");
            match line.rfind(" in ") {
                Some(at) if is_summary => line[..at].to_string(),
                _ => line.to_string(),
            }
        })
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
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("RUST_LOG", "info");
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

fn lock_not_found_error(paths: &IndexPaths) -> String {
    format!(
        "existing index namespace has no permanent lock file at {}; a failed background daemon start can leave this stale shape. Run `codegraph status` from the project root to classify it; only `State: missing` is safe to recover with `codegraph init`.",
        paths.permanent_lock().display()
    )
}

fn lockless_missing_detail(project: &Path, paths: &IndexPaths) -> String {
    format!(
        "CodeGraph index directory exists at {}, but its permanent lock is missing at {}; a failed background daemon start can leave this stale namespace. Run `codegraph init \"{}\"` to create the lock and rebuild the index.",
        paths.current_root().display(),
        paths.permanent_lock().display(),
        project.display()
    )
}

struct LeaseCheckpointBarrier {
    address: SocketAddr,
    arrived: Receiver<(u8, TcpStream)>,
}

impl LeaseCheckpointBarrier {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind lease checkpoint barrier");
        let address = listener.local_addr().expect("lease checkpoint address");
        let (tx, arrived) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut marker = [0_u8; 1];
                if stream.read_exact(&mut marker).is_err() || marker[0] == b'C' {
                    return;
                }
                if tx.send((marker[0], stream)).is_err() {
                    return;
                }
            }
        });
        Self { address, arrived }
    }

    fn configure(&self, command: &mut Command) {
        command
            .env(
                "CODEGRAPH_TEST_LEASE_BARRIER_ADDR",
                self.address.to_string(),
            )
            .env("CODEGRAPH_TEST_LEASE_BARRIER_CHECKPOINT", "handle-opened");
    }

    fn wait_for_handle_opened(&self) -> TcpStream {
        match self.arrived.recv_timeout(Duration::from_secs(10)) {
            Ok((marker, stream)) => {
                assert_eq!(
                    marker, b'H',
                    "existing-root creation must stop before locking"
                );
                stream
            }
            Err(error) => {
                if let Ok(mut cancel) = TcpStream::connect(self.address) {
                    let _ = cancel.write_all(b"C");
                }
                panic!("handle-opened barrier was not reached before its finite deadline: {error}");
            }
        }
    }
}

impl Drop for LeaseCheckpointBarrier {
    fn drop(&mut self) {
        if let Ok(mut cancel) = TcpStream::connect(self.address) {
            let _ = cancel.write_all(b"C");
        }
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn finish(mut self) -> Output {
        self.0
            .take()
            .expect("child still owned")
            .wait_with_output()
            .expect("collect child output")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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

fn slot_bytes(paths: &IndexPaths) -> [Option<Vec<u8>>; 2] {
    paths.state_slots().map(|slot| match std::fs::read(&slot) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read state slot {}: {error}", slot.display()),
    })
}

fn stage_owner_mismatch(dir: &TestDir, label: &str) -> (PathBuf, IndexPaths) {
    let source = dir.path().join(format!("{label}-source"));
    copy_tree(&mini_fixture(), &source);
    let source_arg = source.to_str().expect("UTF-8 source project path");
    let init = run_in(dir.path(), &["init", source_arg]);
    assert!(
        init.ok,
        "setup: source init must succeed: stdout={}, stderr={}",
        init.stdout, init.stderr
    );

    let moved = dir.path().join(format!("{label}-moved"));
    copy_tree(&source, &moved);
    let source_paths = IndexPaths::resolve(&source, None).expect("resolve source index paths");
    let moved_paths = IndexPaths::resolve(&moved, None).expect("resolve moved index paths");
    assert_ne!(
        source_paths.project_identity(),
        moved_paths.project_identity(),
        "copying a project directory must produce a distinct filesystem identity"
    );
    assert!(
        matches!(
            Store::extraction_status(&moved_paths),
            ExtractionStatus::Corrupt {
                reason: CorruptReason::OwnerMismatch { .. }
            }
        ),
        "a copied initialized project must classify as OwnerMismatch"
    );
    (moved, moved_paths)
}

fn owner_mismatch_reason(paths: &IndexPaths) -> CorruptReason {
    match Store::extraction_status(paths) {
        ExtractionStatus::Corrupt {
            reason: reason @ CorruptReason::OwnerMismatch { .. },
        } => reason,
        other => panic!("expected OwnerMismatch, got {other:?}"),
    }
}

fn owner_mismatch_cli_error(project: &Path, reason: &CorruptReason) -> String {
    format!(
        "CodeGraph index state in {} is corrupt: {reason}; the index belongs to a different filesystem location because the project was moved or copied; run `codegraph init {}` to replace it",
        project.display(),
        project.display()
    )
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
fn owner_mismatch_read_and_status_diagnostics_name_explicit_init_recovery() {
    let dir = TestDir::new("owner-mismatch-diagnostics");
    let (project, paths) = stage_owner_mismatch(&dir, "diagnostics");
    let reason = owner_mismatch_reason(&paths);
    let expected_error = owner_mismatch_cli_error(&project, &reason);
    assert_query_error(&dir, &project, &expected_error);

    let project_arg = project.to_str().expect("UTF-8 moved project path");
    let human = run_in(dir.path(), &["status", project_arg]);
    assert!(
        human.ok,
        "human status must describe OwnerMismatch: stdout={}, stderr={}",
        human.stdout, human.stderr
    );
    assert_eq!(normalize(&human.stderr), "");
    assert_eq!(
        human.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   corrupt: {reason}\nDaemon:  stopped\nIndex belongs to a different filesystem location because the project was moved or copied.\nRecovery: run `codegraph init {}` to replace it.\n",
            project.display(),
            paths.current_db().display(),
            project.display()
        )
    );

    let json_run = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(
        json_run.ok,
        "JSON status must describe OwnerMismatch: stdout={}, stderr={}",
        json_run.stdout, json_run.stderr
    );
    assert_eq!(normalize(&json_run.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_run.stdout.trim()).expect("status emits JSON");
    assert_eq!(value["initialized"], false);
    assert_eq!(value["extractionStatus"], "corrupt");
    assert_eq!(
        value["extractionStatusDetail"],
        format!(
            "{reason}; the index belongs to a different filesystem location because the project was moved or copied; run `codegraph init {}` to replace it",
            project.display()
        )
    );
    assert_eq!(
        value["recoveryCommand"],
        format!("codegraph init {}", project.display())
    );
}

#[test]
fn explicit_init_recovers_an_owner_mismatched_namespace() {
    let dir = TestDir::new("owner-mismatch-init");
    let (project, paths) = stage_owner_mismatch(&dir, "init");

    let init = run_in(
        dir.path(),
        &["init", project.to_str().expect("UTF-8 moved project path")],
    );
    assert!(
        init.ok,
        "explicit init must recover OwnerMismatch: stdout={}, stderr={}",
        init.stdout, init.stderr
    );
    assert!(
        !init
            .stderr
            .contains("replaced stale foreign index state slots")
            && !init
                .stderr
                .contains("replaced stale foreign index database")
            && !init.stderr.contains("codegraph_store::rebuild")
            && !init.stderr.contains(" INFO "),
        "normal init progress must not be split by recovery logger output: stderr={}",
        init.stderr
    );
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    let store = Store::open_for_read(&paths, deadline(), || false)
        .expect("recovered OwnerMismatch namespace must be readable");
    assert!(
        store.counts().expect("read recovered counts").node_count > 0,
        "recovered namespace must contain the rebuilt graph"
    );
}

#[test]
fn mixed_owner_mismatch_and_checksum_damage_refuses_init_without_changing_slots() {
    let dir = TestDir::new("owner-mismatch-mixed");
    let (project, paths) = stage_owner_mismatch(&dir, "mixed");
    let [slot0, slot1] = paths.state_slots();
    let mut second: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&slot1).expect("read copied second state slot"))
            .expect("parse copied second state slot");
    second["checksum"] = serde_json::Value::String("0".repeat(64));
    std::fs::write(
        &slot1,
        serde_json::to_vec(&second).expect("serialize checksum-damaged slot"),
    )
    .expect("write checksum-damaged slot");

    let classification = classify(&paths);
    assert!(matches!(
        classification.slot(0),
        SlotOutcome::Invalid(CorruptReason::OwnerMismatch { .. })
    ));
    assert!(matches!(
        classification.slot(1),
        SlotOutcome::Invalid(CorruptReason::ChecksumMismatch { .. })
    ));
    let reason = match classification.status() {
        ExtractionStatus::Corrupt { reason } => reason.clone(),
        other => panic!("mixed slot damage must classify Corrupt, got {other:?}"),
    };
    let before = slot_bytes(&paths);
    let project_arg = project.to_str().expect("UTF-8 moved project path");

    let status = run_in(dir.path(), &["status", project_arg]);
    assert!(status.ok, "status must report mixed corruption");
    assert_eq!(normalize(&status.stderr), "");
    assert_eq!(
        status.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   corrupt: {reason}\nDaemon:  stopped\nManual recovery is required.\n",
            project.display(),
            paths.current_db().display()
        ),
        "a mixed namespace must not recommend init merely because slot 0 supplies the aggregate OwnerMismatch reason"
    );
    let json_status = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(json_status.ok, "JSON status must report mixed corruption");
    assert_eq!(normalize(&json_status.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_status.stdout.trim()).expect("status emits JSON");
    assert_eq!(
        value["extractionStatusDetail"],
        format!("{reason}; manual recovery is required")
    );
    assert!(value.get("recoveryCommand").is_none());

    let init = run_in(dir.path(), &["init", project_arg]);
    assert!(!init.ok, "mixed corruption must refuse explicit init");
    assert_eq!(
        final_error_body(&init.stderr),
        format!("state-gated Store open rejected index state corrupt: {reason}")
    );
    assert_eq!(
        slot_bytes(&paths),
        before,
        "a refused mixed-damage init must preserve both state slots byte-for-byte"
    );
    assert!(slot0.is_file());
}

#[test]
fn another_corrupt_reason_keeps_manual_recovery_and_refuses_init() {
    let dir = TestDir::new("other-corrupt");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("create corrupt project");
    let paths = IndexPaths::resolve(&project, None).expect("resolve corrupt index paths");
    stage_lifecycle_state(&paths, "corrupt");
    let reason = match Store::extraction_status(&paths) {
        ExtractionStatus::Corrupt { reason } => reason,
        other => panic!("expected staged Corrupt state, got {other:?}"),
    };
    assert!(matches!(reason, CorruptReason::EqualSequence { .. }));
    let before = slot_bytes(&paths);
    let project_arg = project.to_str().expect("UTF-8 corrupt project path");

    let human = run_in(dir.path(), &["status", project_arg]);
    assert!(human.ok, "status must report corruption: {}", human.stderr);
    assert_eq!(normalize(&human.stderr), "");
    assert_eq!(
        human.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   corrupt: {reason}\nDaemon:  stopped\nManual recovery is required.\n",
            project.display(),
            paths.current_db().display()
        )
    );
    let json_status = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(json_status.ok, "JSON status must report corruption");
    assert_eq!(normalize(&json_status.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_status.stdout.trim()).expect("status emits JSON");
    assert_eq!(
        value["extractionStatusDetail"],
        format!("{reason}; manual recovery is required")
    );
    assert!(value.get("recoveryCommand").is_none());

    let init = run_in(dir.path(), &["init", project_arg]);
    assert!(!init.ok, "non-OwnerMismatch corruption must refuse init");
    assert_eq!(
        final_error_body(&init.stderr),
        format!("state-gated Store open rejected index state corrupt: {reason}")
    );
    assert_eq!(
        slot_bytes(&paths),
        before,
        "a refused corrupt init must preserve both state slots byte-for-byte"
    );
}

#[test]
fn sync_never_deletes_owner_mismatched_state_slots() {
    let dir = TestDir::new("owner-mismatch-sync");
    let (project, paths) = stage_owner_mismatch(&dir, "sync");
    let reason = owner_mismatch_reason(&paths);
    let before = slot_bytes(&paths);

    let sync = run_in(
        dir.path(),
        &["sync", project.to_str().expect("UTF-8 moved project path")],
    );
    assert!(!sync.ok, "sync must fail closed on OwnerMismatch");
    assert_eq!(
        final_error_body(&sync.stderr),
        owner_mismatch_cli_error(&project, &reason)
    );
    assert_eq!(
        slot_bytes(&paths),
        before,
        "sync must preserve OwnerMismatch state slots byte-for-byte"
    );
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
fn lockless_root_recovery_init_preserves_sentinel_and_builds_current() {
    let dir = TestDir::new("lockless-only-log-init");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve lockless paths");
    std::fs::create_dir(paths.current_root()).expect("create existing index root");
    let sentinel = b"sentinel daemon diagnostic\n";
    std::fs::write(paths.daemon_log(), sentinel).expect("write sentinel daemon log");

    let init = run_in(
        dir.path(),
        &["init", project.to_str().expect("UTF-8 project path")],
    );
    assert!(
        init.ok,
        "lockless Missing root must be recoverable: stdout={}, stderr={}",
        init.stdout, init.stderr
    );
    assert_eq!(
        normalize(&init.stdout),
        format!(
            "Initialized in {}\nIndexed 3 files\n13 nodes, 21 edges",
            project.display()
        )
    );
    assert_eq!(normalize(&init.stderr), "Scanning files…");
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    assert!(paths.permanent_lock().is_file());
    assert!(paths.current_db().is_file());
    assert_eq!(
        std::fs::read(paths.daemon_log()).expect("read preserved daemon log"),
        sentinel
    );
}

#[test]
fn lockless_root_recovery_missing_query_and_status_are_actionable() {
    let dir = TestDir::new("lockless-missing-diagnostics");
    let project = dir.path().join("project");
    std::fs::create_dir(&project).expect("create project");
    let paths = IndexPaths::resolve(&project, None).expect("resolve lockless paths");
    std::fs::create_dir(paths.current_root()).expect("create existing index root");
    std::fs::write(paths.daemon_log(), b"sentinel\n").expect("write sentinel daemon log");
    let detail = lockless_missing_detail(&project, &paths);
    let project_arg = project.to_str().expect("UTF-8 project path");

    assert_query_error(&dir, &project, &detail);

    let human = run_in(dir.path(), &["status", project_arg]);
    assert!(human.ok, "human status must succeed: {}", human.stderr);
    assert_eq!(normalize(&human.stderr), "");
    assert_eq!(
        human.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   missing\nDaemon:  stopped\n{detail}\n",
            project.display(),
            paths.current_db().display()
        )
    );

    let json_run = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(json_run.ok, "JSON status must succeed: {}", json_run.stderr);
    assert_eq!(normalize(&json_run.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_run.stdout.trim()).expect("status emits JSON");
    assert_eq!(value["initialized"], false);
    assert_eq!(value["extractionStatus"], "missing");
    assert_eq!(value["extractionStatusDetail"], detail);
    assert_eq!(
        value["recoveryCommand"],
        format!("codegraph init \"{}\"", project.display())
    );
    assert!(!paths.permanent_lock().exists());
}

#[test]
fn lockless_root_recovery_fresh_missing_keeps_original_diagnostics_and_creates_nothing() {
    let dir = TestDir::new("lockless-fresh-missing");
    let project = dir.path().join("project");
    std::fs::create_dir(&project).expect("create fresh project");
    let paths = IndexPaths::resolve(&project, None).expect("resolve fresh paths");
    let project_arg = project.to_str().expect("UTF-8 project path");
    let query_error = format!(
        "CodeGraph not initialized in {}; run `codegraph init {}` to create or replace the index",
        project.display(),
        project.display()
    );

    assert_query_error(&dir, &project, &query_error);
    assert!(!paths.current_root().exists());

    let human = run_in(dir.path(), &["status", project_arg]);
    assert!(
        human.ok,
        "fresh human status must succeed: {}",
        human.stderr
    );
    assert_eq!(normalize(&human.stderr), "");
    assert_eq!(
        human.stdout,
        format!(
            "\nCodeGraph Status\n\nProject: {}\nDB Path: {}\nState:   missing\nDaemon:  stopped\nNot initialized\nRun \"codegraph init\" to initialize\n",
            project.display(),
            paths.current_db().display()
        )
    );
    assert!(!paths.current_root().exists());

    let json_run = run_in(dir.path(), &["status", "--json", project_arg]);
    assert!(
        json_run.ok,
        "fresh JSON status must succeed: {}",
        json_run.stderr
    );
    assert_eq!(normalize(&json_run.stderr), "");
    let value: serde_json::Value =
        serde_json::from_str(json_run.stdout.trim()).expect("status emits JSON");
    assert_eq!(value["initialized"], false);
    assert_eq!(value["extractionStatus"], "missing");
    assert!(value.get("recoveryCommand").is_none());
    assert!(!paths.current_root().exists());
}

#[test]
fn lockless_root_recovery_refuses_present_future_corrupt_and_current_state() {
    for status in ["future", "corrupt", "current-no-db"] {
        let dir = TestDir::new(&format!("lockless-refuse-{status}"));
        let project = dir.path().join("project");
        std::fs::create_dir(&project).expect("create lifecycle project");
        let paths = IndexPaths::resolve(&project, None).expect("resolve lifecycle paths");
        stage_lifecycle_state(&paths, status);
        let expected_status = Store::extraction_status(&paths);
        let before = slot_bytes(&paths);
        std::fs::remove_file(paths.permanent_lock()).expect("remove permanent lock");

        let init = run_in(
            dir.path(),
            &["init", project.to_str().expect("UTF-8 project path")],
        );
        assert!(!init.ok, "{status} without a lock must refuse init");
        assert_eq!(normalize(&init.stdout), "");
        assert_eq!(final_error_body(&init.stderr), lock_not_found_error(&paths));
        assert_eq!(Store::extraction_status(&paths), expected_status);
        assert_eq!(slot_bytes(&paths), before);
        assert!(!paths.permanent_lock().exists());
        assert!(!paths.current_db().exists());
    }
}

#[test]
fn lockless_root_recovery_race_refuses_state_published_before_kernel_lock() {
    let dir = TestDir::new("lockless-handle-opened-race");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve race paths");
    std::fs::create_dir(paths.current_root()).expect("create existing index root");
    let sentinel = b"sentinel daemon diagnostic\n";
    std::fs::write(paths.daemon_log(), sentinel).expect("write sentinel daemon log");
    std::fs::write(paths.config_toml(), b"[app]\nname = \"codegraph\"\n")
        .expect("write parseable stale config residue");

    let barrier = LeaseCheckpointBarrier::start();
    let mut command = Command::new(bin());
    command
        .current_dir(dir.path())
        .args(["init", project.to_str().expect("UTF-8 project path")])
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", dir.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    barrier.configure(&mut command);
    let child = ChildGuard(Some(command.spawn().expect("spawn barriered init")));

    let mut opened = barrier.wait_for_handle_opened();
    let competitor = IndexLease::acquire_exclusive_existing(&paths, deadline(), || false)
        .expect("competitor acquires the still-unlocked permanent lock");
    publish_index_state(&paths, &competitor, StatePhase::Building)
        .expect("competitor publishes Building");
    let competitor_status = Store::extraction_status(&paths);
    let before = slot_bytes(&paths);
    drop(competitor);
    opened.write_all(b"R").expect("release barriered init");

    let output = child.finish();
    assert!(
        !output.status.success(),
        "takeover must reject state published before it acquired the kernel lock: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(normalize(&String::from_utf8_lossy(&output.stdout)), "");
    assert_eq!(
        final_error_body(&String::from_utf8_lossy(&output.stderr)),
        format!(
            "lockless index takeover at {} no longer classifies missing after acquiring the permanent lock; found {competitor_status}",
            paths.current_root().display()
        )
    );
    assert_eq!(Store::extraction_status(&paths), competitor_status);
    assert_eq!(slot_bytes(&paths), before);
    assert!(!paths.current_db().exists());
    assert_eq!(
        std::fs::read(paths.daemon_log()).expect("read preserved sentinel"),
        sentinel
    );
    assert!(paths.permanent_lock().is_file());
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
