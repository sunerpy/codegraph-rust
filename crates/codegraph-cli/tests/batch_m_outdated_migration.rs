//! Batch M — `incremental_sync_on_outdated_v2_forces_all_files` (plan test 9,
//! frozen plan `upstream-v1.5-portable-fixes.md` lines 557-565 and 750-751).
//!
//! `codegraph sync` must classify the namespace BEFORE mutating a single row.
//! An `Outdated` namespace (built by an older extraction version) therefore
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

#[test]
fn status_on_outdated_namespace_recommends_sync() {
    let dir = TestDir::new("outdated-status-sync");
    let project = init_project("outdated-status-sync", &dir);
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
    stage_state_slot(
        &paths,
        100,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION - 1,
        "current",
    );

    let run = cli(&["status", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "status must report an outdated namespace: stdout={}, stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("State:   outdated")
            && run
                .stdout
                .contains(&format!("codegraph sync {}", project.display())),
        "outdated status must prescribe ordinary sync: {}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("Not initialized") && !run.stdout.contains("codegraph init"),
        "outdated status must not prescribe initialization: {}",
        run.stdout
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
    // The module-surface rows and marker (W4a-F3) are no canonical surface,
    // but the next sync reads them: a migration writes what a full index does.
    let surface_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("js_module_surface")
            .expect("module-surface rows")
    };
    let migrated_rows = surface_rows(&paths);
    assert!(
        migrated_rows
            .iter()
            .any(|(key, _)| key == "js_module_surface_version"),
        "a migration writes the module-surface marker: {migrated_rows:?}"
    );
    assert_eq!(
        migrated_rows,
        surface_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the module-surface rows of `index --force`"
    );
    // The C/C++ dependents marker (W4-CPP-04), likewise.
    let cpp_marker = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("cpp_type_surface")
            .expect("C/C++ dependents marker")
    };
    assert_eq!(
        cpp_marker(&paths),
        vec![("cpp_type_surface_version".to_string(), "2".to_string())],
        "a migration writes the C/C++ dependents marker"
    );
    assert_eq!(
        cpp_marker(&paths),
        cpp_marker(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the C/C++ marker of `index --force`"
    );
    // The case-folded retries' marker (W4-PHP-03), likewise.
    let case_marker = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("case_folded_names")
            .expect("case-folded marker")
    };
    assert_eq!(
        case_marker(&paths),
        vec![("case_folded_names_version".to_string(), "1".to_string())],
        "a migration writes the case-folded marker"
    );
    assert_eq!(
        case_marker(&paths),
        case_marker(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the case-folded marker of `index --force`"
    );
    // The Dart head marker (W4-DART-01), likewise; its rows are
    // `outdated_migration_writes_the_dart_head_rows_of_index_force`'s.
    let dart_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("dart_heads")
            .expect("Dart head rows")
    };
    assert_eq!(
        dart_rows(&paths),
        vec![("dart_heads_version".to_string(), "1".to_string())],
        "a migration writes the Dart head marker"
    );
    assert_eq!(
        dart_rows(&paths),
        dart_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Dart head marker of `index --force`"
    );
    // The Dart library marker (W4-DART-02), likewise; its rows are
    // `outdated_migration_writes_the_dart_library_rows_of_index_force`'s.
    let dart_library_marker = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .get_project_metadata("dart_libraries_version")
            .expect("Dart library marker")
    };
    assert_eq!(
        dart_library_marker(&paths).as_deref(),
        Some("1"),
        "a migration writes the Dart library marker"
    );
    assert_eq!(
        dart_library_marker(&paths),
        dart_library_marker(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Dart library marker of `index --force`"
    );
    // The R base-binding marker (W4-R-01), likewise.
    let r_marker = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("r_base_bindings")
            .expect("R base-binding marker")
    };
    assert_eq!(
        r_marker(&paths),
        vec![("r_base_bindings_version".to_string(), "1".to_string())],
        "a migration writes the R base-binding marker"
    );
    assert_eq!(
        r_marker(&paths),
        r_marker(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the R base-binding marker of `index --force`"
    );
    // The Lua module marker (W4-LUA-01), likewise; its rows are
    // `outdated_migration_writes_the_lua_module_rows_of_index_force`'s.
    let lua_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("lua_module")
            .expect("Lua module rows")
    };
    assert_eq!(
        lua_rows(&paths),
        vec![("lua_modules_version".to_string(), "1".to_string())],
        "a migration writes the Lua module marker"
    );
    assert_eq!(
        lua_rows(&paths),
        lua_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Lua module marker of `index --force`"
    );
    // And the C/C++ namespace rows (W4-CPP-07).
    let namespace_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("cpp_namespace_surface:")
            .expect("C/C++ namespace rows")
    };
    assert_eq!(
        namespace_rows(&paths),
        namespace_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the C/C++ namespace rows of `index --force`"
    );
}

/// W4-JVM-01: a migration writes the Java head rows and their marker a
/// fresh `index --force` writes, so the next sync compares a changed Java
/// file's heads with what it held.
#[test]
fn outdated_migration_writes_the_java_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-java-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/app/Root.java",
            "package app;\n\npublic class Root {\n    protected void assist() { }\n}\n",
        ),
        (
            "src/app/Base.java",
            "package app;\n\npublic class Base extends Root implements Runnable {\n    public void run() { }\n}\n",
        ),
        (
            "src/app/Child.java",
            "package app;\n\npublic class Child extends Base {\n    void go() {\n        assist();\n    }\n}\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-java-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("java_type_heads")
            .expect("Java head rows")
    };
    let migrated = head_rows(&paths);
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "java_type_heads:src/app/Base.java",
            "java_type_heads:src/app/Child.java",
            "java_type_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Java head rows of `index --force`"
    );
}

/// W4-JVM-03: a migration writes the Kotlin surface rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_kotlin_surface_rows_of_index_force() {
    let dir = TestDir::new("outdated-kotlin-surfaces");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/app/Root.kt",
            "package app\n\nopen class Root {\n    fun assist() { }\n}\n",
        ),
        (
            "src/app/Child.kt",
            "package app\n\nclass Child : Root() {\n    fun go() {\n        assist()\n    }\n}\n",
        ),
        (
            "src/app/Dsl.kt",
            "package app\n\ntypealias Block = Root.() -> Unit\n\nfun configure(block: Block) { }\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-kotlin-surfaces", &project);
    let surface_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("kotlin_surface")
            .expect("Kotlin surface rows")
    };
    let migrated = surface_rows(&paths);
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "kotlin_surface:src/app/Child.kt",
            "kotlin_surface:src/app/Dsl.kt",
            "kotlin_surface:src/app/Root.kt",
            "kotlin_surface_version",
        ],
        "a migration writes the surface rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        surface_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Kotlin surface rows of `index --force`"
    );
}

/// W4-SC-01: a migration writes the Scala surface rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_scala_surface_rows_of_index_force() {
    let dir = TestDir::new("outdated-scala-surfaces");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/app/Root.scala",
            "package app\n\ntrait Root {\n  def assist(): Int = 1\n}\n",
        ),
        (
            "src/app/Child.scala",
            "package app\n\nclass Child extends Root {\n  def go(): Int = assist()\n}\n",
        ),
        (
            "src/app/api/package.scala",
            "package app\n\npackage object api extends Root\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-scala-surfaces", &project);
    let surface_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("scala_surface")
            .expect("Scala surface rows")
    };
    let migrated = surface_rows(&paths);
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "scala_surface:src/app/Child.scala",
            "scala_surface:src/app/Root.scala",
            "scala_surface:src/app/api/package.scala",
            "scala_surface_version",
        ],
        "a migration writes the surface rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        surface_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Scala surface rows of `index --force`"
    );
}

/// W4-CS-01: a migration writes the C# surface rows and their marker a fresh
/// `index --force` writes.
#[test]
fn outdated_migration_writes_the_csharp_surface_rows_of_index_force() {
    let dir = TestDir::new("outdated-csharp-surfaces");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/Lib/Base.cs",
            "namespace Lib\n{\n    public class Reader\n    {\n        internal enum State { Start, Done }\n    }\n}\n",
        ),
        (
            "src/Lib/Leaf.cs",
            "namespace Lib\n{\n    public class Leaf : Reader\n    {\n        object Peek() => State.Start;\n    }\n}\n",
        ),
        (
            "src/GlobalUsings.cs",
            "global using Lib;\nglobal using static Lib.Reader;\n",
        ),
        (
            "src/Plain.cs",
            "namespace Lib\n{\n    public class Plain\n    {\n    }\n}\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-csharp-surfaces", &project);
    let surface_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("csharp_surface")
            .expect("C# surface rows")
    };
    let migrated = surface_rows(&paths);
    // `Plain.cs` and `Base.cs` hold no head with a base and no global using.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "csharp_surface:src/GlobalUsings.cs",
            "csharp_surface:src/Lib/Leaf.cs",
            "csharp_surface_version",
        ],
        "a migration writes the surface rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        surface_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the C# surface rows of `index --force`"
    );
}

/// W4-CS-03: a migration writes the member surface rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_member_surface_rows_of_index_force() {
    let dir = TestDir::new("outdated-member-surfaces");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/Json/WriterBase.cs",
            "namespace App.Json;\n\npublic abstract class WriterBase\n{\n    protected JsonWriter Writer { get; set; }\n}\n",
        ),
        (
            "src/main/java/app/WriterBase.java",
            "package app;\n\npublic abstract class WriterBase {\n    protected JsonWriter writer;\n}\n",
        ),
        (
            "src/main/kotlin/app/WriterBase.kt",
            "package app\n\nopen class WriterBase {\n    protected val writer: JsonWriter = JsonWriter()\n}\n",
        ),
        ("src/notes.ts", "export const notes = 1;\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-member-surfaces", &project);
    let surface_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("member_surface")
            .expect("member surface rows")
    };
    let migrated = surface_rows(&paths);
    // No row for the TypeScript file: only C#, Java and Kotlin have one.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "member_surface:src/Json/WriterBase.cs",
            "member_surface:src/main/java/app/WriterBase.java",
            "member_surface:src/main/kotlin/app/WriterBase.kt",
            "member_surface_version",
        ],
        "a migration writes the surface rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        surface_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the member surface rows of `index --force`"
    );
}

/// W4-RB-01: a migration writes the Ruby head rows and their marker a fresh
/// `index --force` writes.
#[test]
fn outdated_migration_writes_the_ruby_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-ruby-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        ("lib/base.rb", "class Base\n  include Helpers\nend\n"),
        (
            "lib/foo.rb",
            "class Foo < Base\n  def run\n    helper()\n  end\nend\n",
        ),
        ("src/notes.ts", "export const notes = 1;\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-ruby-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("ruby_heads")
            .expect("ruby head rows")
    };
    let migrated = head_rows(&paths);
    // No row for the TypeScript file: only Ruby files have one.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "ruby_heads:lib/base.rb",
            "ruby_heads:lib/foo.rb",
            "ruby_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Ruby head rows of `index --force`"
    );
}

/// W4-CF-01: a migration writes the CFML component rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_cfml_component_rows_of_index_force() {
    let dir = TestDir::new("outdated-cfml-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "system/Base.cfc",
            "component {\n  function getSetting( name ) {\n    return 1;\n  }\n}\n",
        ),
        (
            "system/Handler.cfc",
            "component extends=\"app.system.Base\" {\n  function index() {\n    return getSetting( \"x\" );\n  }\n}\n",
        ),
        ("views/main.cfm", "<cfoutput>#now()#</cfoutput>\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-cfml-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("cfml_heads")
            .expect("cfml component rows")
    };
    let migrated = head_rows(&paths);
    // No row for the template: only `.cfc` files have one.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "cfml_heads:system/Base.cfc",
            "cfml_heads:system/Handler.cfc",
            "cfml_heads_version",
        ],
        "a migration writes the component rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the CFML component rows of `index --force`"
    );
}

/// W4-SOL-01: a migration writes the Solidity head rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_solidity_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-solidity-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "contracts/Context.sol",
            "pragma solidity ^0.8.20;\n\nabstract contract Context {\n    function _msgSender() internal view virtual returns (address) {\n        return msg.sender;\n    }\n}\n",
        ),
        (
            "contracts/Governor.sol",
            "pragma solidity ^0.8.20;\n\nimport \"./Context.sol\";\n\nabstract contract Governor is Context {\n    function castVote() public returns (address) {\n        return _msgSender();\n    }\n}\n",
        ),
        (
            "contracts/Free.sol",
            "pragma solidity ^0.8.20;\n\nfunction helper() pure returns (uint256) {\n    return 1;\n}\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-solidity-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("solidity_heads")
            .expect("solidity head rows")
    };
    let migrated = head_rows(&paths);
    // No row for the file with a free function only: it declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "solidity_heads:contracts/Context.sol",
            "solidity_heads:contracts/Governor.sol",
            "solidity_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Solidity head rows of `index --force`"
    );
}

/// W4-LIQ-01: a migration writes the Shopify theme row and its marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_shopify_theme_row_of_index_force() {
    let dir = TestDir::new("outdated-shopify-themes");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "layout/theme.liquid",
            "{% section 'main' %}{{ content_for_layout }}\n",
        ),
        ("sections/main.liquid", "<main></main>\n"),
        ("shop/config/settings_schema.json", "[]"),
        ("shop/sections/main.liquid", "<main></main>\n"),
        (
            "shop/templates/index.json",
            "{\"sections\":{\"main\":{\"type\":\"main\"}},\"order\":[\"main\"]}",
        ),
        ("extensions/reviews/blocks/stars.liquid", "<svg></svg>\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-shopify-themes", &project);
    let theme_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("shopify_themes")
            .expect("shopify theme rows")
    };
    let migrated = theme_rows(&paths);
    // No root for the app extension's block: it is in no theme.
    assert_eq!(
        migrated,
        [
            ("shopify_themes".to_string(), "[\"\",\"shop\"]".to_string()),
            ("shopify_themes_version".to_string(), "1".to_string()),
        ],
        "a migration writes the theme row and its marker"
    );
    assert_eq!(
        migrated,
        theme_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Shopify theme row of `index --force`"
    );
}

/// W4-SW-01: a migration writes the Swift head rows and their marker a
/// fresh `index --force` writes.
#[test]
fn outdated_migration_writes_the_swift_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-swift-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "App/Card.swift",
            "import SwiftUI\nextension View {\n    func cardStyle() -> some View { padding() }\n}\n",
        ),
        (
            "App/HomeView.swift",
            "import SwiftUI\nstruct HomeView: View {\n    var body: some View { Text(\"x\") }\n}\n",
        ),
        ("App/main.swift", "print(1)\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-swift-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("swift_heads")
            .expect("swift head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "swift_heads:App/Card.swift",
            "swift_heads:App/HomeView.swift",
            "swift_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Swift head rows of `index --force`"
    );
}

/// W4-GO-01: a migration writes the Go head rows (each type declaration's
/// embeddings, mentions and digest) and their marker as `index --force`
/// does.
#[test]
fn outdated_migration_writes_the_go_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-go-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        ("go.mod", "module example.com/app\n\ngo 1.22\n"),
        (
            "h/server.go",
            "package h\n\ntype server struct {\n\tBaseAPI\n\tstore *store\n}\n",
        ),
        ("h/run.go", "package h\n\nfunc (s *server) Run() {}\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-go-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("go_heads")
            .expect("go head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["go_heads:h/server.go", "go_heads_version"],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Go head rows of `index --force`"
    );
}

/// W4-RS-03: a migration writes the Rust receiver marker as `index --force`
/// does, so the next sync does not re-resolve every Rust file again.
#[test]
fn outdated_migration_writes_the_rust_receiver_marker_of_index_force() {
    let dir = TestDir::new("outdated-rust-receivers");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "pub struct Widget;\n\npub fn run() {\n    let _ = Vec::<u8>::new();\n}\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-rust-receivers", &project);
    let marker = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("rust_receivers")
            .expect("rust receiver marker")
    };
    let migrated = marker(&paths);
    assert_eq!(
        migrated,
        [("rust_receivers_version".to_string(), "1".to_string())],
        "a migration writes the marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        marker(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the marker of `index --force`"
    );
}

/// W4-SW-04: a migration writes the Objective-C head rows (a class's
/// written superclass, a property's declared type) and their marker as
/// `index --force` does.
#[test]
fn outdated_migration_writes_the_objc_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-objc-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "Core/SDCache.h",
            "@interface SDCache : SDBase\n@property (nonatomic) NSURL *url;\n@end\n",
        ),
        (
            "Core/SDCache.m",
            "#import \"SDCache.h\"\n@implementation SDCache\n- (void)run {\n    [self prepare];\n}\n@end\n",
        ),
        ("Core/util.m", "int twice(int x) {\n    return x * 2;\n}\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-objc-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("objc_heads")
            .expect("objc head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no class or property.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "objc_heads:Core/SDCache.h",
            "objc_heads:Core/SDCache.m",
            "objc_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Objective-C head rows of `index --force`"
    );
}

/// W4-DART-01: a migration writes the Dart head rows (each type
/// declaration's name, whether it is an extension and the supertypes its
/// head names) and their marker as `index --force` does.
#[test]
fn outdated_migration_writes_the_dart_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-dart-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        ("pubspec.yaml", "name: app\n"),
        (
            "lib/cubit.dart",
            "class Cubit extends Base with Mixed {}\n\nextension Helpers on Cubit {\n  void helper() {}\n}\n",
        ),
        ("lib/main.dart", "void main() {}\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-dart-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("dart_heads")
            .expect("Dart head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["dart_heads:lib/cubit.dart", "dart_heads_version"],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert!(
        migrated[0].1.contains("\"Base\",\"Mixed\"")
            && migrated[0].1.contains("\"extension\":true"),
        "a row names the supertypes and the extension: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Dart head rows of `index --force`"
    );
}

/// W4-DART-02: a migration writes the Dart library rows (a file's `library`
/// name, `part of` target, parts, and imports and exports with their
/// prefixes and combinators) and their marker as `index --force` does.
#[test]
fn outdated_migration_writes_the_dart_library_rows_of_index_force() {
    let dir = TestDir::new("outdated-dart-libraries");
    let project = dir.path().join("mini");
    for (path, contents) in [
        ("pubspec.yaml", "name: app\n"),
        (
            "lib/app.dart",
            "library app;\n\nimport 'src/util.dart' as util show helper;\nexport 'src/util.dart' hide hidden;\n\npart 'piece.dart';\n",
        ),
        ("lib/piece.dart", "part of 'app.dart';\n\nint run() => 1;\n"),
        (
            "lib/src/util.dart",
            "int helper() => 1;\nint hidden() => 2;\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-dart-libraries", &project);
    let library_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("dart_librar")
            .expect("Dart library rows")
    };
    let migrated = library_rows(&paths);
    // No row for a file without a directive.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "dart_libraries_version",
            "dart_library:lib/app.dart",
            "dart_library:lib/piece.dart"
        ],
        "a migration writes the library rows and their marker: {migrated:?}"
    );
    assert!(
        migrated[1].1.contains("\"prefix\":\"util\"")
            && migrated[1].1.contains("\"show\":[\"helper\"]")
            && migrated[1].1.contains("\"hide\":[\"hidden\"]")
            && migrated[1].1.contains("\"parts\":[\"piece.dart\"]"),
        "a row records the directives: {migrated:?}"
    );
    assert_eq!(
        migrated,
        library_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Dart library rows of `index --force`"
    );
}

/// W4-LUA-01: a migration writes the Lua module rows (a file's locals, the
/// modules its aliases `require`, what it reads through a global table and a
/// digest of what an alias through it reads) and their marker as `index
/// --force` does.
#[test]
fn outdated_migration_writes_the_lua_module_rows_of_index_force() {
    let dir = TestDir::new("outdated-lua-modules");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "kong/pdk/phases.lua",
            "local function check_phase(accepted)\n  return accepted\nend\n\nreturn {\n  check = check_phase,\n}\n",
        ),
        (
            "kong/handler.lua",
            "local phases = require \"kong.pdk.phases\"\nlocal cp = phases.check\nlocal snooze = ngx.sleep\n\nlocal function run()\n  cp(\"access\")\n  snooze(1)\nend\n\nreturn { run = run }\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-lua-modules", &project);
    let module_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("lua_module")
            .expect("Lua module rows")
    };
    let migrated = module_rows(&paths);
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "lua_module:kong/handler.lua",
            "lua_module:kong/pdk/phases.lua",
            "lua_modules_version",
        ],
        "a migration writes the module rows and their marker: {migrated:?}"
    );
    assert!(
        migrated[0].1.contains("\"kong.pdk.phases\"") && migrated[0].1.contains("\"sleep\""),
        "a row names the modules its aliases require and what it reads through a \
         global table: {migrated:?}"
    );
    assert_eq!(
        migrated,
        module_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the Lua module rows of `index --force`"
    );
}

/// W4-PHP-01: a migration writes the PHP head rows (a type's kind and the
/// supertypes its `extends` and trait `use`s name, through its file's
/// imports) and their marker as `index --force` does.
#[test]
fn outdated_migration_writes_the_php_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-php-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "src/Child.php",
            "<?php\nnamespace App;\n\nuse Vendor\\Base;\n\nclass Child extends Base\n{\n    use Greets;\n\n    public function run()\n    {\n        $this->prepare();\n    }\n}\n",
        ),
        (
            "src/Greets.php",
            "<?php\nnamespace App;\n\ntrait Greets\n{\n    public function hello() {}\n}\n",
        ),
        (
            "src/helpers.php",
            "<?php\nfunction twice($x)\n{\n    return $x * 2;\n}\n",
        ),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-php-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("php_heads")
            .expect("php head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "php_heads:src/Child.php",
            "php_heads:src/Greets.php",
            "php_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert!(
        migrated[0].1.contains("Vendor::Base") && migrated[0].1.contains("App::Greets"),
        "a row names the supertypes through the file's imports: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the PHP head rows of `index --force`"
    );
}

/// W5-07: a migration writes the class-head rows (each type declaration's
/// language, name and the supertypes its own head writes) and their marker
/// as `index --force` does.
#[test]
fn outdated_migration_writes_the_class_head_rows_of_index_force() {
    let dir = TestDir::new("outdated-class-heads");
    let project = dir.path().join("mini");
    for (path, contents) in [
        (
            "pkg/user.py",
            "from pkg.base import Base\n\n\nclass User(Base):\n    pass\n",
        ),
        (
            "pkg/base.py",
            "class Base:\n    @classmethod\n    def create(cls):\n        return cls()\n",
        ),
        ("pkg/helpers.py", "def twice(x):\n    return x * 2\n"),
    ] {
        let full = project.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("create the source directory");
        fs::write(full, contents).expect("write a source file");
    }
    let run = cli(&["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed (stdout={}, stderr={})",
        run.stdout, run.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve v2 paths");
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
    let (_scratch, peer) = fresh_force_peer("outdated-class-heads", &project);
    let head_rows = |paths: &IndexPaths| {
        Store::open_for_read(paths, deadline(), || false)
            .expect("readable namespace")
            .project_metadata_with_prefix("class_heads")
            .expect("class head rows")
    };
    let migrated = head_rows(&paths);
    // No row for a file that declares no type.
    assert_eq!(
        migrated
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [
            "class_heads:pkg/base.py",
            "class_heads:pkg/user.py",
            "class_heads_version",
        ],
        "a migration writes the head rows and their marker: {migrated:?}"
    );
    assert!(
        migrated[1].1.contains("\"User\"") && migrated[1].1.contains("\"Base\""),
        "a row names the type and the supertype its head writes: {migrated:?}"
    );
    assert_eq!(
        migrated,
        head_rows(&IndexPaths::resolve(&peer, None).expect("resolve peer v2 paths")),
        "a forced migration must write the class-head rows of `index --force`"
    );
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
