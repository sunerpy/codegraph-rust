//! Batch M — `incremental_sync_on_outdated_v2_forces_all_files` (plan test 9,
//! frozen plan `upstream-v1.5-portable-fixes.md` lines 557-565 and 750-751).
//!
//! `codegraph sync` must classify the namespace BEFORE mutating a single row.
//! An `Outdated` v2 namespace (built by an older extraction version) therefore
//! cannot be updated file-by-file: the sync escalates, under the SAME retained
//! exclusive lease, to a deterministic full from-source migration that bypasses
//! every mtime/content-hash skip, processes every current candidate in sorted
//! order, drops tracked files that no longer exist, reruns framework extraction /
//! resolution / maintenance, and publishes `phase=current` last. Its five
//! canonical surfaces equal a fresh v2 `index --force`.
//!
//! `Future` and `Corrupt` states are refused with ZERO bytes changed anywhere in
//! the index namespace, and an `uninitialized` namespace stays reserved for an
//! explicit `init`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};
use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, Store, checksum_hex,
};

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
            "codegraph-batchm-migration-{label}-{}-{}",
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

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn cli(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
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
        .expect("migration test deadline")
}

/// Author ONE authoritative fixed state slot with an explicit protocol/version/
/// phase triple and remove the companion, so classification is unambiguous. The
/// checksum comes from the shipped canonical payload helper, so the staged slot
/// is a genuine protocol record rather than a hand-rolled approximation.
fn stage_state_slot(
    paths: &IndexPaths,
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
) {
    let identity = paths.project_identity();
    let checksum = checksum_hex(
        sequence,
        storage_protocol,
        extraction_version,
        phase,
        identity,
    );
    let body = format!(
        "{{\"sequence\":{sequence},\"storageProtocol\":{storage_protocol},\
         \"extractionVersion\":{extraction_version},\"phase\":\"{phase}\",\
         \"projectIdentity\":\"{identity}\",\"checksum\":\"{checksum}\"}}\n"
    );
    let [slot0, slot1] = paths.state_slots();
    fs::write(&slot0, body).expect("stage authoritative state slot");
    let _ = fs::remove_file(&slot1);
}

/// Complete no-follow snapshot entry for the refused-sync nonmutation oracle.
#[derive(Debug, PartialEq, Eq)]
enum NamespaceEntry {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

/// Fail-closed snapshot of the complete index namespace. Directories are data
/// (an empty directory has no other payload), regular files retain every byte,
/// and aliases retain only their target without following it. Any I/O failure or
/// unsupported entry kind aborts the proof instead of silently disappearing from
/// both sides of the comparison.
fn namespace_snapshot(root: &Path) -> BTreeMap<PathBuf, NamespaceEntry> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, NamespaceEntry>) {
        let mut paths = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("snapshot read_dir {}: {error}", dir.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("snapshot entry in {}: {error}", dir.display()))
                    .path()
            })
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|error| panic!("snapshot strip {}: {error}", path.display()))
                .to_path_buf();
            let metadata = fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("snapshot metadata {}: {error}", path.display()));
            let file_type = metadata.file_type();
            let entry =
                if file_type.is_dir() {
                    NamespaceEntry::Directory
                } else if file_type.is_file() {
                    NamespaceEntry::File(fs::read(&path).unwrap_or_else(|error| {
                        panic!("snapshot read {}: {error}", path.display())
                    }))
                } else if file_type.is_symlink() {
                    NamespaceEntry::Symlink(fs::read_link(&path).unwrap_or_else(|error| {
                        panic!("snapshot read_link {}: {error}", path.display())
                    }))
                } else {
                    panic!("snapshot unsupported entry kind: {}", path.display());
                };
            assert!(
                out.insert(relative, entry).is_none(),
                "duplicate namespace snapshot path"
            );
            if file_type.is_dir() {
                walk(root, &path, out);
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Compare snapshots without dumping database bytes into a failure message.
fn assert_namespace_unchanged(
    before: &BTreeMap<PathBuf, NamespaceEntry>,
    after: &BTreeMap<PathBuf, NamespaceEntry>,
    label: &str,
) {
    let mut paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let changed = paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "a refused {label} sync changed namespace entries: {changed:?}"
    );
}

/// `Synced: N reindexed, M skipped (unchanged), K removed in …`
fn parse_sync_counters(stdout: &str) -> (usize, usize, usize) {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("Synced: "))
        .unwrap_or_else(|| panic!("sync must print its counters, got: {stdout}"));
    let number_before = |needle: &str| -> usize {
        let head = line
            .split(needle)
            .next()
            .unwrap_or_else(|| panic!("missing {needle} in {line}"));
        head.rsplit(|c: char| !c.is_ascii_digit())
            .find(|token| !token.is_empty())
            .unwrap_or_else(|| panic!("no count before {needle} in {line}"))
            .parse()
            .unwrap_or_else(|_| panic!("unparsable count before {needle} in {line}"))
    };
    (
        number_before(" reindexed"),
        number_before(" skipped"),
        number_before(" removed"),
    )
}

fn init_project(label: &str, dir: &TestDir) -> PathBuf {
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup {label}: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    project
}

/// A fresh, fully-indexed peer of `project` built by `init` + `index --force`,
/// used as the canonical migration oracle.
fn fresh_force_peer(label: &str, project: &Path) -> (TestDir, PathBuf) {
    let scratch = TestDir::new(&format!("{label}-scratch"));
    let peer = scratch.path().join("mini");
    copy_tree(project, &peer);
    let index_root = IndexPaths::resolve(&peer, None)
        .expect("resolve peer v2 paths")
        .current_root()
        .to_path_buf();
    let _ = fs::remove_dir_all(&index_root);
    let run = cli(&["init", peer.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup {label}: peer init must succeed: {} {}",
        run.stdout, run.stderr
    );
    let run = cli(&["index", "--force", peer.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup {label}: peer `index --force` must succeed: {} {}",
        run.stdout, run.stderr
    );
    (scratch, peer)
}

/// Guard the nonmutation oracle itself: an equal-length in-place write must make
/// its assertion fail, so the refusal tests cannot regress to a size-only proof.
#[test]
fn namespace_snapshot_detects_equal_length_byte_mutation() {
    let dir = TestDir::new("snapshot-self-test");
    let root = dir.path().join("index");
    fs::create_dir(&root).expect("create snapshot root");
    let file = root.join("state.json");
    fs::write(&file, b"AAAA").expect("write original bytes");
    let before = namespace_snapshot(&root);
    fs::write(&file, b"BBBB").expect("replace with equal-length bytes");
    let after = namespace_snapshot(&root);

    let detected = std::panic::catch_unwind(|| {
        assert_namespace_unchanged(&before, &after, "oracle-self-test")
    });
    assert!(
        detected.is_err(),
        "the namespace nonmutation oracle must reject equal-length byte changes"
    );
}

/// Plan test 9: an `Outdated` namespace forces EVERY file through migration —
/// zero unchanged skips even though every source byte is identical to the
/// outdated database — and the result equals a fresh v2 `index --force`.
#[test]
fn incremental_sync_on_outdated_v2_forces_all_files() {
    let dir = TestDir::new("outdated-forces-all");
    let project = init_project("outdated-forces-all", &dir);
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");

    // Every source file is byte-identical to what the index already holds, so an
    // unguarded incremental sync would skip all of them on the hash gate.
    let tracked = {
        let store = Store::open_for_read(&paths, deadline(), || false)
            .expect("the initialized namespace is readable");
        store
            .all_files()
            .expect("tracked files")
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>()
    };
    assert!(
        tracked.len() >= 2,
        "the mini fixture must track several files, got {tracked:?}"
    );

    // The namespace was built by an OLDER extraction version.
    stage_state_slot(
        &paths,
        100,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION - 1,
        "current",
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Outdated {
            built: CURRENT_EXTRACTION_VERSION - 1,
        },
        "setup: the staged slot must classify Outdated"
    );

    let run = cli(&["sync", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "sync must migrate an Outdated namespace: stdout={}, stderr={}",
        run.stdout, run.stderr
    );

    let (reindexed, skipped, _removed) = parse_sync_counters(&run.stdout);
    assert_eq!(
        skipped, 0,
        "an Outdated namespace must bypass every mtime/content-hash skip \
         (plan lines 557-565); got {skipped} unchanged skips in: {}",
        run.stdout
    );
    assert_eq!(
        reindexed,
        tracked.len(),
        "migration must process EVERY current candidate, got {reindexed} of {} in: {}",
        tracked.len(),
        run.stdout
    );

    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "migration must publish phase=current as its last step"
    );
    Store::open_for_read(&paths, deadline(), || false)
        .expect("a migrated namespace must be readable through the state gate");

    let (_scratch, peer) = fresh_force_peer("outdated-forces-all", &project);
    let migrated = canonicalize_db(&paths.current_db()).expect("canonicalize migrated db");
    let rebuilt = canonicalize_db(
        &IndexPaths::resolve(&peer, None)
            .expect("resolve peer v2 paths")
            .current_db(),
    )
    .expect("canonicalize peer db");
    diff_canonical(&rebuilt, &migrated, None)
        .expect("a forced migration must equal a fresh v2 `index --force`");
}

/// Migration is a from-source rebuild, so a tracked file that no longer exists
/// on disk disappears from the migrated index and the result still equals a
/// fresh `index --force` over the surviving tree.
#[test]
fn outdated_migration_drops_absent_tracked_files() {
    let dir = TestDir::new("outdated-drops-absent");
    let project = init_project("outdated-drops-absent", &dir);
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");

    let removed_source = project.join("src/math.ts");
    assert!(
        removed_source.is_file(),
        "setup: the mini fixture must contain src/math.ts"
    );
    fs::remove_file(&removed_source).expect("delete a tracked source file");

    stage_state_slot(
        &paths,
        100,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION - 1,
        "current",
    );

    let run = cli(&["sync", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "sync must migrate an Outdated namespace: stdout={}, stderr={}",
        run.stdout, run.stderr
    );

    let store = Store::open_for_read(&paths, deadline(), || false)
        .expect("a migrated namespace must be readable");
    assert!(
        store
            .file_by_path("src/math.ts")
            .expect("query tracked file")
            .is_none(),
        "migration must drop tracked files that no longer exist on disk"
    );
    drop(store);

    let (_scratch, peer) = fresh_force_peer("outdated-drops-absent", &project);
    let migrated = canonicalize_db(&paths.current_db()).expect("canonicalize migrated db");
    let rebuilt = canonicalize_db(
        &IndexPaths::resolve(&peer, None)
            .expect("resolve peer v2 paths")
            .current_db(),
    )
    .expect("canonicalize peer db");
    diff_canonical(&rebuilt, &migrated, None)
        .expect("a migration that dropped an absent file must equal a fresh `index --force`");
}

/// A `Future` or `Corrupt` namespace is refused by the sync writer gate before
/// any row mutation, leaving every byte in the namespace unchanged.
#[test]
fn sync_refuses_future_and_corrupt_state_without_mutation() {
    for label in ["future", "corrupt"] {
        let dir = TestDir::new(&format!("refuse-{label}"));
        let project = init_project(label, &dir);
        let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");

        if label == "future" {
            stage_state_slot(
                &paths,
                100,
                CURRENT_STORAGE_PROTOCOL,
                CURRENT_EXTRACTION_VERSION + 1,
                "current",
            );
        } else {
            let [slot0, slot1] = paths.state_slots();
            fs::write(&slot0, b"{ not a state record").expect("stage a corrupt slot");
            let _ = fs::remove_file(&slot1);
        }
        let status = Store::extraction_status(&paths);
        assert!(
            matches!(
                status,
                ExtractionStatus::Future { .. } | ExtractionStatus::Corrupt { .. }
            ),
            "setup {label}: staged slot must classify Future/Corrupt, got {status:?}"
        );

        let before = namespace_snapshot(paths.current_root());
        let run = cli(&["sync", project.to_str().unwrap()]);
        assert!(
            !run.ok,
            "sync must refuse a {label} namespace: stdout={}, stderr={}",
            run.stdout, run.stderr
        );
        let after = namespace_snapshot(paths.current_root());
        assert_namespace_unchanged(&before, &after, label);
    }
}

/// An interrupted-`uninit` namespace stays reserved for an explicit `init`: a
/// sync neither continues it nor mutates it.
#[test]
fn sync_refuses_uninitialized_namespace_without_mutation() {
    let dir = TestDir::new("refuse-uninit");
    let project = init_project("refuse-uninit", &dir);
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");

    stage_state_slot(
        &paths,
        100,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "uninitialized",
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Uninitialized,
        "setup: the staged slot must classify Uninitialized"
    );

    let before = namespace_snapshot(paths.current_root());
    let run = cli(&["sync", project.to_str().unwrap()]);
    assert!(
        !run.ok,
        "sync must refuse an uninitialized namespace: stdout={}, stderr={}",
        run.stdout, run.stderr
    );
    let after = namespace_snapshot(paths.current_root());
    assert_namespace_unchanged(&before, &after, "uninitialized");
}
