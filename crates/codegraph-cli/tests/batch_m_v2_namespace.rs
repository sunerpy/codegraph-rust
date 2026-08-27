//! Isolation mirrors `cli_commands.rs`: a private temp project plus an isolated
//! `CODEGRAPH_HTTP_REGISTRY_DIR`, and `CODEGRAPH_NO_DAEMON=1` so no daemon
//! rendezvous state leaks. The default `init` target is `none`, so no agent
//! config is written.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_core::IndexPaths;
use codegraph_store::{ExtractionStatus, Store};
use serde_json::Value;

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
            "codegraph-batchm-{label}-{}-{}",
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

fn run_in(registry_dir: &Path, args: &[&str]) -> Run {
    run_in_env(registry_dir, args, &[])
}

fn run_in_env(registry_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("RUST_LOG", "info");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn stage_foreign_database(paths: &IndexPaths) {
    let foreign = Store::open(&paths.current_db()).expect("create a real foreign SQLite index");
    foreign
        .set_project_metadata("foreign_fixture", "must be replaced")
        .expect("write a foreign-only marker");
    foreign
        .restore_default_pragmas()
        .expect("checkpoint foreign DB");
    drop(foreign);

    assert_eq!(
        Store::extraction_status(paths),
        ExtractionStatus::Missing,
        "a real pre-state-slot database must classify Missing"
    );
    assert!(paths.current_db().is_file(), "foreign SQLite DB exists");
    assert!(
        paths.state_slots().iter().all(|slot| !slot.exists()),
        "foreign fixture must have no state slots"
    );
}

#[test]
fn init_writes_default_project_local_codegraph_root() {
    let dir = TestDir::new("default-root");
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

    let selected_db = project.join(".codegraph/codegraph.db");
    let retired_db = project.join(".codegraph-v2/codegraph.db");
    let built_bytes = std::fs::read(&selected_db).unwrap_or_default();
    assert!(
        !built_bytes.is_empty(),
        "`init` must produce a non-empty index DB at {}",
        selected_db.display()
    );
    assert!(
        !retired_db.exists(),
        "`init` must not recreate the retired namespace at {}",
        retired_db.display()
    );
}

/// A real pre-state-slot SQLite index in the selected root is a rebuildable
/// foreign cache, not an unrecoverable contradiction. `init` must acquire its
/// one rebuild lease and replace that database automatically without injecting
/// raw logger output into the normal progress UI.
#[test]
fn init_takes_over_real_sqlite_database_without_state_slots() {
    let dir = TestDir::new("foreign-db-takeover");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve selected index root");

    stage_foreign_database(&paths);

    let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "init must automatically replace a Missing-state SQLite index: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        !run.stderr.contains("replaced stale foreign index database")
            && !run.stderr.contains("codegraph_store::rebuild")
            && !run.stderr.contains(" INFO "),
        "normal init progress must not be split by recovery logger output: stderr={}",
        run.stderr
    );

    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    let rebuilt = Store::open_for_read(
        &paths,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
        || false,
    )
    .expect("taken-over index is readable");
    assert_eq!(
        rebuilt
            .get_project_metadata("foreign_fixture")
            .expect("query foreign-only marker"),
        None,
        "takeover must rebuild instead of adopting foreign rows"
    );
}

#[test]
fn foreign_database_replacement_details_require_debug_logging() {
    let dir = TestDir::new("foreign-db-debug-log");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve selected index root");
    stage_foreign_database(&paths);

    let run = run_in_env(
        dir.path(),
        &["init", project.to_str().unwrap()],
        &[("RUST_LOG", "codegraph_store::rebuild=debug")],
    );
    assert!(
        run.ok,
        "debug init must still replace a Missing-state SQLite index: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains("replaced stale foreign index database")
            && run
                .stderr
                .contains(&paths.current_db().display().to_string())
            && run.stderr.contains("missing state slots")
            && run.stderr.contains("DEBUG"),
        "explicit debug logging must retain replacement diagnostics: stderr={}",
        run.stderr
    );
}

#[test]
fn status_json_reports_foreign_database_without_opening_or_mutating_it() {
    let dir = TestDir::new("foreign-db-status-json");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve selected index root");
    stage_foreign_database(&paths);
    let before = tree_snapshot(&project);

    let run = run_in(dir.path(), &["status", "--json", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "status --json must degrade instead of failing: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    let status: Value = serde_json::from_str(run.stdout.trim()).expect("valid status JSON");
    let actual_keys = status
        .as_object()
        .expect("status JSON object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_keys = [
        "daemonLogPath",
        "daemonPidPath",
        "daemonRunning",
        "daemonSocketPath",
        "dbExists",
        "dbPath",
        "dbSizeBytes",
        "extractionStatus",
        "extractionStatusDetail",
        "indexPath",
        "initialized",
        "lastIndexed",
        "legacyIndexPaths",
        "legacyIndexPresent",
        "projectPath",
        "version",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(actual_keys, expected_keys, "degraded status key set");
    assert_eq!(status["initialized"], Value::Bool(false));
    assert_eq!(status["dbExists"], Value::Bool(true));
    assert_eq!(status["extractionStatus"], Value::String("missing".into()));
    assert_eq!(status["lastIndexed"], Value::Null);
    assert!(status["dbSizeBytes"].as_u64().is_some_and(|size| size > 0));
    assert!(
        status["extractionStatusDetail"]
            .as_str()
            .is_some_and(
                |detail| detail.contains("no state slots") && detail.contains("codegraph init")
            ),
        "diagnostic must explain the state and remedy: {status}"
    );
    assert_tree_bytes_unchanged(&before, &tree_snapshot(&project), "degraded JSON status");
}

#[test]
fn status_human_reports_foreign_database_and_init_remedy_without_mutation() {
    let dir = TestDir::new("foreign-db-status-human");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve selected index root");
    stage_foreign_database(&paths);
    let before = tree_snapshot(&project);

    let run = run_in(dir.path(), &["status", project.to_str().unwrap()]);
    assert!(
        run.ok,
        "human status must degrade instead of failing: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("index database has no state slots")
            && run.stdout.contains("older version or another tool")
            && run.stdout.contains("codegraph init")
            && run.stdout.contains("replace"),
        "human status must name the situation and remedy: stdout={} stderr={}",
        run.stdout,
        run.stderr
    );
    assert_tree_bytes_unchanged(&before, &tree_snapshot(&project), "degraded human status");
}

#[test]
fn foreign_database_command_errors_name_explicit_init_without_mutation() {
    let dir = TestDir::new("foreign-db-command-remedy");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let paths = IndexPaths::resolve(&project, None).expect("resolve selected index root");
    stage_foreign_database(&paths);
    let before = tree_snapshot(&project);
    let p = project.to_str().unwrap();

    for (command, run) in [
        ("query", run_in(dir.path(), &["query", "a", "--path", p])),
        ("sync", run_in(dir.path(), &["sync", p])),
    ] {
        assert!(
            !run.ok,
            "{command} must reject a database without state slots: stdout={} stderr={}",
            run.stdout, run.stderr
        );
        let output = format!("{}{}", run.stdout, run.stderr);
        assert!(
            output.contains("codegraph init") && output.contains(p),
            "{command} must name the explicit recovery command and project: {output}"
        );
    }

    assert_tree_bytes_unchanged(
        &before,
        &tree_snapshot(&project),
        "foreign database command rejection",
    );
}

/// A healthy state-slot-backed index is already authoritative. Re-running
/// `init` must take the read-only early return and leave both database and state
/// bytes untouched, proving the foreign-index takeover cannot fire on Current.
#[test]
fn init_does_not_rebuild_a_current_index() {
    let dir = TestDir::new("current-not-rebuilt");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let first = run_in(dir.path(), &["init", p]);
    assert!(
        first.ok,
        "initial init must succeed: {} {}",
        first.stdout, first.stderr
    );
    let paths = IndexPaths::resolve(&project, None).expect("resolve current index");
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    let before_db = std::fs::read(paths.current_db()).expect("snapshot current DB");
    let before_slots = paths
        .state_slots()
        .map(|slot| std::fs::read(slot).expect("snapshot current state slot"));

    let second = run_in(dir.path(), &["init", p]);
    assert!(
        second.ok && second.stdout.contains("Already initialized"),
        "second init must take the Current early return: {} {}",
        second.stdout,
        second.stderr
    );
    assert_eq!(
        std::fs::read(paths.current_db()).expect("read current DB after second init"),
        before_db,
        "Current database bytes must not be rebuilt"
    );
    assert_eq!(
        paths
            .state_slots()
            .map(|slot| std::fs::read(slot).expect("read current slot after second init")),
        before_slots,
        "Current state-slot bytes must not be republished"
    );
}

#[test]
fn configured_relative_root_uses_project_local_join_via_cli() {
    let dir = TestDir::new("cfg-rel");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let run = run_in_env(dir.path(), &["init", p], &[("CODEGRAPH_DIR", "cache")]);
    assert!(run.ok, "init must succeed: {} {}", run.stdout, run.stderr);

    assert!(
        project.join("cache/codegraph.db").is_file(),
        "configured root must be the direct project-local join"
    );
    assert!(!project.join(".codegraph/codegraph.db").exists());

    let status = run_in_env(
        dir.path(),
        &["status", "--json", p],
        &[("CODEGRAPH_DIR", "cache")],
    );
    assert!(status.ok, "status must succeed: {}", status.stderr);
    let status_json: Value = serde_json::from_str(status.stdout.trim()).expect("valid status JSON");
    let status_db = Path::new(
        status_json["dbPath"]
            .as_str()
            .expect("status dbPath must be a string"),
    );
    assert_eq!(
        (
            status_db
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            status_db.file_name().and_then(|name| name.to_str()),
        ),
        (Some("cache"), Some("codegraph.db")),
        "status dbPath must end in the configured project-local cache/codegraph.db: {}",
        status.stdout
    );
}

#[test]
fn absolute_configured_root_fails_closed_without_mutation_via_cli() {
    let dir = TestDir::new("cfg-abs");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let shared = dir.path().join("shared/cg");
    std::fs::create_dir_all(dir.path().join("shared")).unwrap();
    let shared_str = shared.to_str().unwrap();
    let before = tree_snapshot(&project);

    let run = run_in_env(
        dir.path(),
        &["init", project.to_str().unwrap()],
        &[("CODEGRAPH_DIR", shared_str)],
    );
    assert!(
        !run.ok,
        "absolute CODEGRAPH_DIR must fail closed: {} {}",
        run.stdout, run.stderr
    );
    assert!(
        format!("{}{}", run.stdout, run.stderr)
            .contains("must be one non-empty project-local directory name"),
        "absolute-root failure must be actionable: {} {}",
        run.stdout,
        run.stderr
    );
    assert!(
        !shared.exists(),
        "external configured root must not be created"
    );
    let after = tree_snapshot(&project);
    assert_tree_bytes_unchanged(&before, &after, "an absolute-root rejection");
}

#[test]
fn configured_dot_alias_fails_closed_without_mutation_via_cli() {
    let dir = TestDir::new("cfg-dot");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let run = run_in_env(dir.path(), &["init", p], &[("CODEGRAPH_DIR", ".")]);
    assert!(
        !run.ok,
        "init with CODEGRAPH_DIR=. must fail closed: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        !project.join("codegraph.db").is_file(),
        "a `.` alias must not write `<project>/codegraph.db`"
    );
    assert!(
        !project.join(".codegraph").join("codegraph.db").is_file(),
        "a `.` alias must not create the default index"
    );
}

/// The exact, deterministic representation of ONE filesystem entry in the
/// nonmutation oracle. Every supported entry kind carries its complete payload,
/// so equality of two snapshots is real evidence — not a proxy:
///
/// - [`EntryKind::Directory`] — presence itself is the payload, so creating or
///   removing an EMPTY directory is detectable (a file-only snapshot misses it).
/// - [`EntryKind::RegularFile`] — the COMPLETE bytes, never the length: an
///   equal-length in-place write keeps the size identical, so size equality is
///   NOT evidence of byte identity.
/// - [`EntryKind::Symlink`] — the link TARGET, read with `read_link`; the link
///   is never followed, so the oracle neither reads through it nor mistakes a
///   change of the pointed-to file for a mutation of this tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    Directory,
    RegularFile(Vec<u8>),
    Symlink(PathBuf),
}

/// One snapshot entry: the OS-native relative path (a [`PathBuf`], so the
/// equality key is never a lossy `to_string_lossy` rendering) plus its exact
/// payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TreeEntry {
    rel: PathBuf,
    kind: EntryKind,
}

/// A bounded, byte-free label for a failure message: names the KIND (and a
/// length or link target), never the file contents.
fn kind_label(kind: &EntryKind) -> String {
    match kind {
        EntryKind::Directory => "directory".to_string(),
        EntryKind::RegularFile(bytes) => format!("file[{} bytes]", bytes.len()),
        EntryKind::Symlink(target) => format!("symlink -> {}", target.display()),
    }
}

/// Recursively snapshot EVERY filesystem entry under `root` — directories,
/// regular files (complete bytes), and symlinks (their targets) — sorted, so a
/// command can be proven nonmutating by comparing the before/after sets.
///
/// FAIL-CLOSED: every I/O step is unwrapped with an explicit panic instead of
/// being skipped or defaulted. A swallowed `read_dir`/entry error would silently
/// drop a whole subtree from BOTH snapshots (making a mutation inside it
/// invisible), and `fs::read(..).unwrap_or_default()` would map an unreadable
/// file to empty bytes on both sides — each turns a real mutation into a false
/// "unchanged". An entry kind with no deterministic exact representation (fifo,
/// socket, device, …) panics rather than being silently omitted.
fn tree_snapshot(root: &Path) -> Vec<TreeEntry> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<TreeEntry>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
            panic!(
                "nonmutation oracle: read_dir({}) failed: {e} — the oracle must fail \
                 loudly, never silently skip a subtree",
                dir.display()
            )
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| {
                panic!(
                    "nonmutation oracle: a directory entry of {} could not be read: {e}",
                    dir.display()
                )
            });
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap_or_else(|_| {
                    panic!(
                        "nonmutation oracle: {} is not under the snapshot base {}",
                        path.display(),
                        base.display()
                    )
                })
                .to_path_buf();
            // `symlink_metadata` describes the LINK itself, so a symlink is never
            // resolved to its destination here.
            let meta = std::fs::symlink_metadata(&path).unwrap_or_else(|e| {
                panic!(
                    "nonmutation oracle: symlink_metadata({}) failed: {e}",
                    path.display()
                )
            });
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                let target = std::fs::read_link(&path).unwrap_or_else(|e| {
                    panic!(
                        "nonmutation oracle: read_link({}) failed: {e}",
                        path.display()
                    )
                });
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::Symlink(target),
                });
            } else if file_type.is_dir() {
                // Record the directory ITSELF before descending, so an empty
                // directory's creation/removal is visible.
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::Directory,
                });
                walk(&path, base, out);
            } else if file_type.is_file() {
                let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                    panic!(
                        "nonmutation oracle: read({}) failed: {e} — an unreadable file \
                         must not be recorded as empty bytes",
                        path.display()
                    )
                });
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::RegularFile(bytes),
                });
            } else {
                panic!(
                    "nonmutation oracle: unsupported entry kind at {} ({file_type:?}); no \
                     deterministic exact representation exists, so the oracle refuses to \
                     omit it",
                    path.display()
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Assert two [`tree_snapshot`]s are EXACTLY equal, reporting only the CHANGED
/// entries (created / removed / same-path-different-payload). A bare `assert_eq!`
/// on the snapshots would dump every file's full contents into the failure
/// message; this compares the same exact payloads but names just the offending
/// paths and kinds.
fn assert_tree_bytes_unchanged(before: &[TreeEntry], after: &[TreeEntry], context: &str) {
    let mut diffs: Vec<String> = Vec::new();
    let before_map: std::collections::BTreeMap<&Path, &EntryKind> =
        before.iter().map(|e| (e.rel.as_path(), &e.kind)).collect();
    let after_map: std::collections::BTreeMap<&Path, &EntryKind> =
        after.iter().map(|e| (e.rel.as_path(), &e.kind)).collect();
    for (path, kind) in &before_map {
        match after_map.get(path) {
            None => diffs.push(format!(
                "removed: {} ({})",
                path.display(),
                kind_label(kind)
            )),
            Some(other) if other != kind => diffs.push(format!(
                "changed: {} ({} -> {})",
                path.display(),
                kind_label(kind),
                kind_label(other)
            )),
            Some(_) => {}
        }
    }
    for (path, kind) in &after_map {
        if !before_map.contains_key(path) {
            diffs.push(format!(
                "created: {} ({})",
                path.display(),
                kind_label(kind)
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "{context}: project tree must be EXACTLY unchanged (complete file bytes, \
         directory presence, and symlink targets compared — never sizes), but \
         changed: {diffs:?}"
    );
}

/// `status` under an invalid/aliasing `CODEGRAPH_DIR` must FAIL CLOSED through
/// the REAL CLI — surfacing the stable diagnostic instead of masking the bad
/// configuration behind a default `.codegraph` "not initialized" report — and
/// must leave the project tree byte-for-byte unchanged (a read command never
/// mutates). This is the CLI-side proof of the status fail-closed correction.
#[test]
fn status_fails_closed_on_invalid_configured_root_without_mutation_via_cli() {
    let dir = TestDir::new("status-invalid");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let before = tree_snapshot(&project);

    for json in [false, true] {
        let mut args = vec!["status"];
        if json {
            args.push("--json");
        }
        args.push(p);
        let run = run_in_env(dir.path(), &args, &[("CODEGRAPH_DIR", ".")]);
        assert!(
            !run.ok,
            "status (json={json}) with CODEGRAPH_DIR=. must fail closed, not report a \
             default layout: stdout={} stderr={}",
            run.stdout, run.stderr
        );
        assert!(
            !run.stdout.contains("Not initialized"),
            "status must NOT mask an invalid configured root as the default layout: stdout={}",
            run.stdout
        );
        // The actionable `IndexPaths` diagnostic must reach the user (on stderr,
        // where the CLI prints `Error: …`).
        let combined = format!("{}{}", run.stdout, run.stderr);
        assert!(
            combined.contains("must be one non-empty project-local directory name"),
            "status (json={json}) must surface the stable unsafe-root diagnostic \
             for `.`: stdout={} stderr={}",
            run.stdout,
            run.stderr
        );
    }

    let after = tree_snapshot(&project);
    assert_tree_bytes_unchanged(&before, &after, "a fail-closed `status`");
}

/// The byte snapshot must catch an EQUAL-LENGTH in-place mutation — the exact
/// hole a size-only snapshot left open. Self-test of the harness: mutating one
/// byte without changing any file length must be reported as changed.
#[test]
fn tree_snapshot_detects_equal_length_byte_mutation() {
    let dir = TestDir::new("snap-selftest");
    let project = dir.path().join("mini");
    std::fs::create_dir_all(&project).unwrap();
    let victim = project.join("a.txt");
    std::fs::write(&victim, b"AAAA").unwrap();

    let before = tree_snapshot(&project);
    std::fs::write(&victim, b"AAAB").unwrap();
    let after = tree_snapshot(&project);

    assert_eq!(
        before.len(),
        after.len(),
        "sanity: the mutation must keep the file set identical"
    );
    let byte_len = |entry: &TreeEntry| match &entry.kind {
        EntryKind::RegularFile(bytes) => bytes.len(),
        other => panic!("sanity: the victim must be a regular file, got {other:?}"),
    };
    assert_eq!(
        byte_len(&before[0]),
        byte_len(&after[0]),
        "sanity: the mutation must keep the byte LENGTH identical, so only a \
         full-byte comparison can detect it"
    );
    assert_ne!(
        before, after,
        "a same-length byte mutation must change the snapshot"
    );
    assert_oracle_rejects(&before, &after, "a same-length byte mutation");
}

/// The oracle must FAIL on the mutation described by `what`. Wrapped in
/// `catch_unwind` because [`assert_tree_bytes_unchanged`] proves itself by
/// panicking; a silent pass here would mean the oracle degraded again.
fn assert_oracle_rejects(before: &[TreeEntry], after: &[TreeEntry], what: &str) {
    let outcome = std::panic::catch_unwind(|| {
        assert_tree_bytes_unchanged(before, after, "self-test");
    });
    assert!(
        outcome.is_err(),
        "the nonmutation assertion must FAIL on {what}"
    );
}

/// Creating or removing an EMPTY directory must be detected. A file-only
/// snapshot records nothing for an empty directory, so such a mutation would be
/// invisible — the oracle therefore snapshots directories themselves.
#[test]
fn tree_snapshot_detects_empty_directory_mutation() {
    let dir = TestDir::new("snap-emptydir");
    let project = dir.path().join("mini");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), b"AAAA").unwrap();

    let before = tree_snapshot(&project);
    let empty = project.join("scratch");
    std::fs::create_dir(&empty).unwrap();
    let after_create = tree_snapshot(&project);

    assert_ne!(
        before, after_create,
        "creating an EMPTY directory must change the snapshot (no file changed)"
    );
    assert_oracle_rejects(&before, &after_create, "an empty-directory creation");

    std::fs::remove_dir(&empty).unwrap();
    let after_remove = tree_snapshot(&project);
    assert_eq!(
        before, after_remove,
        "removing the empty directory must restore the exact snapshot"
    );
    assert_oracle_rejects(&after_create, &after_remove, "an empty-directory removal");
}

/// A symlink is snapshotted as its TARGET, never followed. Retargeting the link
/// mutates this tree and must be detected; and because the link is not followed,
/// a change to the pointed-to file OUTSIDE the tree must NOT be reported as a
/// mutation of the tree.
#[cfg(unix)]
#[test]
fn tree_snapshot_detects_symlink_target_mutation_without_following() {
    let dir = TestDir::new("snap-symlink");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let target_a = outside.join("a.bin");
    let target_b = outside.join("b.bin");
    std::fs::write(&target_a, b"AAAA").unwrap();
    std::fs::write(&target_b, b"BBBB").unwrap();

    let project = dir.path().join("mini");
    std::fs::create_dir_all(&project).unwrap();
    let link = project.join("link");
    std::os::unix::fs::symlink(&target_a, &link).unwrap();

    let before = tree_snapshot(&project);
    assert_eq!(
        before,
        vec![TreeEntry {
            rel: PathBuf::from("link"),
            kind: EntryKind::Symlink(target_a.clone()),
        }],
        "a symlink must be recorded as its target, not as the pointed-to bytes"
    );

    // Retarget the link: the tree itself changed.
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&target_b, &link).unwrap();
    let retargeted = tree_snapshot(&project);
    assert_ne!(
        before, retargeted,
        "retargeting a symlink must change the snapshot"
    );
    assert_oracle_rejects(&before, &retargeted, "a symlink retarget");

    // Mutating the pointed-to file outside the tree must NOT read through the
    // link, so the tree snapshot stays identical.
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&target_a, &link).unwrap();
    let restored = tree_snapshot(&project);
    assert_eq!(before, restored, "sanity: the link points at A again");
    std::fs::write(&target_a, b"ZZZZ").unwrap();
    let after_outside_write = tree_snapshot(&project);
    assert_eq!(
        restored, after_outside_write,
        "the oracle must NOT follow the link: an outside-the-tree write is not a \
         mutation of this tree"
    );
    assert_tree_bytes_unchanged(&restored, &after_outside_write, "self-test");
}

/// An entry with no deterministic exact representation must make the oracle
/// PANIC, never be silently omitted: a skipped entry disappears from BOTH
/// snapshots, so a mutation of it would read as "unchanged". A unix domain
/// socket file is the portable-in-std way to create such an entry.
#[cfg(unix)]
#[test]
fn tree_snapshot_fails_loudly_on_unsupported_entry_kind() {
    let dir = TestDir::new("snap-special");
    let project = dir.path().join("mini");
    std::fs::create_dir_all(&project).unwrap();
    let sock = project.join("s.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

    let outcome = std::panic::catch_unwind(|| tree_snapshot(&project));
    let message = outcome
        .err()
        .map(|payload| match payload.downcast::<String>() {
            Ok(s) => *s,
            Err(_) => "<non-string panic>".to_string(),
        })
        .expect("the oracle must PANIC on an unsupported entry kind, not skip it");
    assert!(
        message.contains("unsupported entry kind"),
        "the panic must name the unsupported entry kind: {message}"
    );
}

#[test]
fn escaping_relative_root_fails_closed_without_mutation_via_cli() {
    let dir = TestDir::new("cfg-escape");
    let base = dir.path();
    let project = base.join("project");
    copy_tree(&mini_fixture(), &project);
    std::fs::create_dir_all(base.join("shared")).unwrap();
    let before = tree_snapshot(&project);

    let run = run_in_env(
        base,
        &["init", project.to_str().unwrap()],
        &[("CODEGRAPH_DIR", "../shared/cg")],
    );
    assert!(
        !run.ok,
        "escaping CODEGRAPH_DIR must fail closed: {} {}",
        run.stdout, run.stderr
    );
    assert!(
        format!("{}{}", run.stdout, run.stderr)
            .contains("must be one non-empty project-local directory name"),
        "escaping-root failure must be actionable: {} {}",
        run.stdout,
        run.stderr
    );
    assert!(!base.join("shared/cg").exists());
    let after = tree_snapshot(&project);
    assert_tree_bytes_unchanged(&before, &after, "an escaping-root rejection");
}
