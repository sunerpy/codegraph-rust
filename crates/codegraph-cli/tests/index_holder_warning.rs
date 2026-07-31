//! `codegraph index` — the PRE-REBUILD warning about stdio MCP servers that may
//! hold this project's index database open (P1-b of the rev55 plan).
//!
//! A full rebuild deletes `codegraph.db`/`-wal`/`-shm` before writing the new
//! index. Unix can unlink an open file; Windows cannot, which is why the partner
//! team's `index --force` failed while stray `codegraph serve --mcp` processes
//! were holding the database. The fix for the ROOT cause shipped in v0.41.0
//! (the MCP engine is request-scoped now); what this suite pins is the remaining
//! legibility gap — telling the user WHO may be holding the index, BEFORE the
//! destructive step, and never changing the exit code.
//!
//! Seeding uses the test process's own pid, which is guaranteed alive, so the
//! entry survives the read-time liveness prune. `CODEGRAPH_MCP_REGISTRY_DIR`
//! keeps every case off a developer's real state directory.
//!
//! This lives in its own file rather than in `mcp_registry_cli.rs`: that suite
//! documents the `mcp list` READ surface, whereas these cases drive `index`.
//! Nothing is shared between integration test files in this crate, so the small
//! harness below is duplicated by the same convention every sibling follows.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn mini_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .join("crates/codegraph-bench/fixtures/mini")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-idxwarn-{label}-{}-{}",
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

/// Copy the mini fixture to `<dir>/<name>` and give it a v2 index namespace.
fn indexed_project(dir: &TestDir, name: &str) -> PathBuf {
    let project = dir.path().join(name);
    copy_tree(&mini_fixture(), &project);
    let status = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    project
}

/// Write one `<pid>.json` registry entry in the registry's own camelCase shape.
/// `project` of `None` is the `serve --mcp` launched WITHOUT `--path` case (the
/// Kiro / Qoder shape), which resolves its project per request.
fn seed_entry(registry_dir: &Path, pid: u32, project: Option<&str>) {
    std::fs::create_dir_all(registry_dir).unwrap();
    let mut entry = json!({
        "pid": pid,
        "transport": "stdio",
        "startedAt": 1_700_000_000_123u64,
        "version": "0.41.0",
    });
    if let Some(project) = project {
        entry["project"] = json!(project);
    }
    std::fs::write(
        registry_dir.join(format!("{pid}.json")),
        format!("{entry}\n"),
    )
    .unwrap();
}

struct Run {
    stdout: String,
    stderr: String,
    success: bool,
}

fn run_index(registry_dir: &Path, project: &Path, extra: &[&str]) -> Run {
    let mut args: Vec<&str> = vec!["index", project.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = Command::new(bin())
        .args(&args)
        .env("CODEGRAPH_MCP_REGISTRY_DIR", registry_dir)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run codegraph index");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        success: out.status.success(),
    }
}

/// The wording that only ever appears when a holder was found.
const WARNING_MARKER: &str = "may hold this project's index database open";

/// Drop the two fields that are volatile BETWEEN RUNS even without this feature:
/// the subscriber's startup timestamp on stderr, and the elapsed-duration tail of
/// the summary on stdout. What remains must match byte-for-byte, which is how the
/// "successful output is unchanged" claim is checked without a stored baseline.
fn normalize(stream: &str) -> String {
    stream
        .lines()
        .filter(|line| !line.contains("logger initialized"))
        .map(|line| match line.find(" in ") {
            Some(at) if line.ends_with("ms") => line[..at].to_string(),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A live server registered against THIS project is warned about before the
/// destructive rebuild — and the warning is advisory only: `index` still
/// succeeds, still prints its summary, and still exits 0.
#[test]
fn warns_before_rebuild_when_a_server_is_registered_for_this_project() {
    let dir = TestDir::new("same-project");
    let registry_dir = dir.path().join("mcp-registry");
    let project = indexed_project(&dir, "proj");
    let pid = std::process::id();
    seed_entry(&registry_dir, pid, Some(project.to_str().unwrap()));

    let run = run_index(&registry_dir, &project, &[]);

    assert!(
        run.success,
        "the warning must NOT change the exit code: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains(WARNING_MARKER),
        "index must warn that a registered stdio MCP server may hold the DB: stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.contains(&pid.to_string()),
        "the warning must name the holder's pid ({pid}): stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.contains("Scanning files"),
        "the warning must come BEFORE the rebuild, not replace its output: stderr={}",
        run.stderr
    );
    assert!(
        run.stdout.contains("Indexed"),
        "the rest of the success path is untouched: stdout={}",
        run.stdout
    );

    // `--quiet` is the machine-readable mode; `cmd_index` gates all of its other
    // human-facing output on it, and this warning follows that convention.
    let quiet = run_index(&registry_dir, &project, &["--quiet"]);
    assert!(quiet.success, "--quiet index must still exit 0");
    assert!(
        !quiet.stderr.contains(WARNING_MARKER),
        "--quiet must suppress the advisory warning: stderr={}",
        quiet.stderr
    );
}

/// A server with NO pinned project (`serve --mcp` without `--path`) resolves its
/// project per request, so it CAN hold this project's database — excluding it
/// would hide exactly the Kiro-style launches this warning exists for.
#[test]
fn warns_for_a_server_with_no_pinned_project() {
    let dir = TestDir::new("no-project");
    let registry_dir = dir.path().join("mcp-registry");
    let project = indexed_project(&dir, "proj");
    seed_entry(&registry_dir, std::process::id(), None);

    let run = run_index(&registry_dir, &project, &[]);

    assert!(run.success, "still exits 0: stderr={}", run.stderr);
    assert!(
        run.stderr.contains(WARNING_MARKER) && run.stderr.contains("--path"),
        "a project-less server must be warned about and explained: stderr={}",
        run.stderr
    );
}

/// An EMPTY registry and a registry holding only an UNRELATED project's server
/// both leave the successful `index` output unchanged — no warning, and the
/// normalized streams match the empty-registry baseline byte-for-byte.
#[test]
fn unrelated_project_and_empty_registry_leave_output_unchanged() {
    let dir = TestDir::new("unrelated");
    let project = indexed_project(&dir, "proj");
    let unrelated = dir.path().join("other-proj");

    let empty_dir = dir.path().join("registry-empty");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let baseline = run_index(&empty_dir, &project, &[]);
    assert!(
        baseline.success,
        "baseline index must succeed: stderr={}",
        baseline.stderr
    );
    assert!(
        !baseline.stderr.contains(WARNING_MARKER),
        "an empty registry has nothing to say: stderr={}",
        baseline.stderr
    );

    let seeded_dir = dir.path().join("registry-unrelated");
    seed_entry(
        &seeded_dir,
        std::process::id(),
        Some(unrelated.to_str().unwrap()),
    );
    let seeded = run_index(&seeded_dir, &project, &[]);
    assert!(
        seeded.success,
        "index must succeed: stderr={}",
        seeded.stderr
    );
    assert!(
        !seeded.stderr.contains(WARNING_MARKER),
        "a server for an UNRELATED project must not raise a false alarm: stderr={}",
        seeded.stderr
    );
    assert_eq!(
        normalize(&seeded.stderr),
        normalize(&baseline.stderr),
        "stderr must be unchanged when there is no holder to report"
    );
    assert_eq!(
        normalize(&seeded.stdout),
        normalize(&baseline.stdout),
        "stdout must be unchanged when there is no holder to report"
    );
}
