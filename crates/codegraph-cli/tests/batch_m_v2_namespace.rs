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
