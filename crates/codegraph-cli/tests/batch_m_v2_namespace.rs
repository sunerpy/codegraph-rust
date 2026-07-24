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
