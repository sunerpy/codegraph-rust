//! Batch M — initial black-box Red for the isolated v2 index namespace.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md` (Batch M, "Product boundary and
//! selected storage layout", plan line ~262) makes an explicit product-level
//! compatibility break: the default *current* index root is
//! `project/.codegraph-v2`, a **sibling** of the fixed legacy `project/.codegraph`
//! root, and new binaries never open, migrate, or write the legacy namespace
//! (plan lines 288-291). Acceptance for the Red boundary (plan lines 805-817)
//! requires black-box evidence that current v0.40.4 behavior "opens/writes the
//! fixed legacy `.codegraph` namespace instead of an isolated `.codegraph-v2`
//! namespace".
//!
//! This is the INITIAL black-box Red described at plan lines 805-809: it uses
//! ONLY the shipped public CLI surface (`codegraph init`) and filesystem
//! artifacts. It imports no proposed Green type (`IndexPaths`, `IndexLease`,
//! `open_for_*`) — those are Green design, not compile-time Red prerequisites.
//! It is NOT the later API-level refinement of the store/lease/path modes.
//!
//! Isolation mirrors `cli_commands.rs`: a private temp project plus an isolated
//! `CODEGRAPH_HTTP_REGISTRY_DIR`, and `CODEGRAPH_NO_DAEMON=1` so no daemon
//! rendezvous state leaks. The default `init` target is `none`, so no agent
//! config is written.

use std::path::{Path, PathBuf};
use std::process::Command;

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
        .env("CODEGRAPH_NO_DAEMON", "1");
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

fn indexed_names(project: &Path) -> Vec<String> {
    std::fs::read_dir(project)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Initial Batch M black-box Red: `init` must build the index in the isolated
/// sibling `.codegraph-v2` namespace, never the fixed legacy `.codegraph` one.
///
/// Expected Red on the v0.40.4 base: `init` writes `.codegraph/codegraph.db`
/// (the fixed legacy root) and never creates `.codegraph-v2`, so the named
/// v2-namespace assertion fails behaviorally. The setup (`init` succeeds) and
/// the byte snapshot below both reach the assertion, so this is a behavioral —
/// not a compile/setup/panic — failure. The snapshot preserves the built DB
/// bytes that later Green will assert remain a byte-usable legacy graph.
#[test]
fn init_writes_isolated_v2_namespace_not_legacy_codegraph() {
    let dir = TestDir::new("v2-namespace");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    // Public shipped surface only.
    let run = run_in(dir.path(), &["init", p]);
    assert!(
        run.ok,
        "setup: `codegraph init` must succeed before the behavioral assertion \
         (stdout={}, stderr={})",
        run.stdout, run.stderr
    );

    let legacy_root = project.join(".codegraph");
    let legacy_db = legacy_root.join("codegraph.db");
    let v2_root = project.join(".codegraph-v2");
    let v2_db = v2_root.join("codegraph.db");

    // Byte snapshot of the DB the build actually produced (whichever namespace
    // it landed in). Later Green preserves this as the byte-usable legacy graph;
    // recording it here proves the build produced a non-empty DB and that the
    // failure below is a namespace-placement failure, not an empty/failed build.
    let built_db = if v2_db.is_file() {
        v2_db.clone()
    } else {
        legacy_db.clone()
    };
    let built_bytes = std::fs::read(&built_db).unwrap_or_default();
    assert!(
        !built_bytes.is_empty(),
        "setup: `init` must produce a non-empty index DB (checked {}); \
         got legacy_db.is_file()={}, v2_db.is_file()={}",
        built_db.display(),
        legacy_db.is_file(),
        v2_db.is_file()
    );

    // PRIMARY behavioral Red assertion (plan line 262 + lines 805-817): the DB
    // must live in the isolated sibling v2 namespace, not the legacy root.
    assert!(
        v2_db.is_file(),
        "Batch M: `init` must create the isolated v2 namespace at {} \
         (a sibling of the legacy root, per plan line 262), but no v2 DB exists; \
         current v0.40.4 behavior wrote the legacy namespace instead \
         (.codegraph/codegraph.db present={})",
        v2_db.display(),
        legacy_db.is_file()
    );

    // Secondary behavioral Red assertion (plan lines 288-291): new binaries must
    // never write the fixed legacy namespace. Reached only once the v2 assertion
    // passes at Green; both are Green-stable (legacy root absent post-Green).
    assert!(
        !legacy_db.is_file(),
        "Batch M: a new binary must not write the legacy namespace at {}; \
         the fixed legacy `.codegraph` root is read-only to new binaries",
        legacy_db.display()
    );
}

/// The v2 current root for a relative `CODEGRAPH_DIR` must be the fail-closed
/// `IndexPaths::resolve` identity-suffixed SIBLING `<name>-v2-<projectIdentity>`
/// through the REAL CLI, never the old `<project>/<value>` simple-join. Proven
/// by `status --json`'s `indexPath` and the on-disk DB placement after `init`.
#[test]
fn configured_relative_root_uses_identity_suffixed_sibling_via_cli() {
    let dir = TestDir::new("cfg-rel");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let p = project.to_str().unwrap();

    let run = run_in_env(dir.path(), &["init", p], &[("CODEGRAPH_DIR", "cache")]);
    assert!(run.ok, "init must succeed: {} {}", run.stdout, run.stderr);

    let names = indexed_names(&project);
    // The simple-join `<project>/cache/codegraph.db` MUST NOT exist.
    assert!(
        !project.join("cache").join("codegraph.db").is_file(),
        "configured root must NOT use the old simple-join `cache/`: dir={names:?}"
    );
    // Exactly one identity-suffixed sibling `cache-v2-<64hex>` holds the DB.
    let sibling = names
        .iter()
        .find(|n| n.starts_with("cache-v2-"))
        .unwrap_or_else(|| panic!("expected a `cache-v2-<identity>` sibling, got {names:?}"));
    assert_eq!(
        sibling.len(),
        "cache-v2-".len() + 64,
        "sibling must carry a full 64-hex projectIdentity: {sibling}"
    );
    assert!(
        project.join(sibling).join("codegraph.db").is_file(),
        "the identity-suffixed sibling must hold the DB: {names:?}"
    );

    let status = run_in_env(
        dir.path(),
        &["status", "--json", p],
        &[("CODEGRAPH_DIR", "cache")],
    );
    assert!(status.ok, "status must succeed: {}", status.stderr);
    assert!(
        status.stdout.contains(sibling.as_str()),
        "status indexPath must name the identity sibling {sibling}: {}",
        status.stdout
    );
}

/// Two DISTINCT physical projects given the SAME absolute `CODEGRAPH_DIR` must
/// receive DISTINCT identity-suffixed current roots through the REAL CLI, so one
/// project can never open the other's index.
#[test]
fn two_projects_sharing_absolute_configured_root_get_distinct_roots_via_cli() {
    let dir = TestDir::new("cfg-abs");
    let project_a = dir.path().join("a/mini");
    let project_b = dir.path().join("b/mini");
    copy_tree(&mini_fixture(), &project_a);
    copy_tree(&mini_fixture(), &project_b);
    let shared = dir.path().join("shared/cg");
    std::fs::create_dir_all(dir.path().join("shared")).unwrap();
    let shared_str = shared.to_str().unwrap();

    let ra = run_in_env(
        dir.path(),
        &["init", project_a.to_str().unwrap()],
        &[("CODEGRAPH_DIR", shared_str)],
    );
    assert!(ra.ok, "init a must succeed: {} {}", ra.stdout, ra.stderr);
    let rb = run_in_env(
        dir.path(),
        &["init", project_b.to_str().unwrap()],
        &[("CODEGRAPH_DIR", shared_str)],
    );
    assert!(rb.ok, "init b must succeed: {} {}", rb.stdout, rb.stderr);

    let parent = dir.path().join("shared");
    let siblings: Vec<String> = std::fs::read_dir(&parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("cg-v2-"))
        .collect();
    assert_eq!(
        siblings.len(),
        2,
        "two projects must produce two distinct identity siblings, got {siblings:?}"
    );
    assert_ne!(
        siblings[0], siblings[1],
        "the two current roots must differ"
    );
    // The shared simple-join `shared/cg/codegraph.db` must never exist.
    assert!(
        !shared.join("codegraph.db").is_file(),
        "configured absolute root must NOT collapse two projects onto one simple-join DB"
    );
}

/// A `CODEGRAPH_DIR` that aliases the project root (`.`) must fail closed
/// through the REAL CLI, creating NO `<project>/codegraph.db` and NO legacy
/// mutation — the fail-closed `resolve` contract, not the old simple-join.
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
        "a `.` alias must not mutate the legacy namespace"
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
/// configuration behind a default `.codegraph-v2` "not initialized" report — and
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
            !run.stdout.contains(".codegraph-v2"),
            "status must NOT mask an invalid configured root as a `.codegraph-v2` \
             default: stdout={}",
            run.stdout
        );
        // The actionable `IndexPaths` diagnostic must reach the user (on stderr,
        // where the CLI prints `Error: …`). Assert the STABLE reason phrasing —
        // `.` aliases the project root — not merely "some error".
        let combined = format!("{}{}", run.stdout, run.stderr);
        assert!(
            combined.contains("project root itself"),
            "status (json={json}) must surface the stable unsafe-root diagnostic \
             (the `.` alias resolves to the project root itself): stdout={} stderr={}",
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

/// An escaping relative `CODEGRAPH_DIR` (`../shared/cg`) is a VALID configured
/// root that escapes the project; two sibling projects given the same escaping
/// value must each `init` into their own identity-suffixed sibling under the
/// shared external parent, never a single shared simple-join DB — the REAL-CLI
/// counterpart of the absolute-root isolation case.
#[test]
fn two_projects_sharing_escaping_relative_root_get_distinct_roots_via_cli() {
    let dir = TestDir::new("cfg-escape");
    let base = dir.path();
    let project_a = base.join("a");
    let project_b = base.join("b");
    copy_tree(&mini_fixture(), &project_a);
    copy_tree(&mini_fixture(), &project_b);
    std::fs::create_dir_all(base.join("shared")).unwrap();

    for project in [&project_a, &project_b] {
        let run = run_in_env(
            base,
            &["init", project.to_str().unwrap()],
            &[("CODEGRAPH_DIR", "../shared/cg")],
        );
        assert!(
            run.ok,
            "init with escaping CODEGRAPH_DIR must succeed for {}: {} {}",
            project.display(),
            run.stdout,
            run.stderr
        );
    }

    let shared_parent = base.join("shared");
    let siblings: Vec<String> = std::fs::read_dir(&shared_parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("cg-v2-"))
        .collect();
    assert_eq!(
        siblings.len(),
        2,
        "two projects sharing an escaping relative root must produce two distinct \
         identity siblings, got {siblings:?}"
    );
    assert_ne!(
        siblings[0], siblings[1],
        "the two current roots must differ"
    );
    assert!(
        !shared_parent.join("cg").join("codegraph.db").is_file(),
        "the escaping simple-join `shared/cg/codegraph.db` must never exist"
    );
}
