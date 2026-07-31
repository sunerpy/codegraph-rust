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

/// The wording that only ever appears when at least one live server was found.
const WARNING_MARKER: &str = "may be holding this index database open";

/// Vocabulary that belongs exclusively to the holder guidance, used to prove the
/// pre-warning contributes ZERO bytes when nothing is registered.
const GUIDANCE_VOCABULARY: [&str; 3] = ["stdio MCP", "0.40.x", "serve --mcp"];

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

/// A server registered for ANOTHER project must STILL be warned about. The
/// registry's `project` field is only the launch-time default, never a capability
/// boundary: `resolve_project_arg` (`codegraph-mcp/src/roots.rs`) pushes an
/// absolute per-call `projectPath` straight into its candidate list and probes it
/// on its own merits, consulting the launch default ONLY when no path was passed.
/// A server launched in A therefore opens B's database the moment a client asks
/// it to — which is exactly the partner-reported Windows rebuild failure. Warning
/// about it is the whole point of this check, so over-warning is the correct bias.
#[test]
fn warns_when_a_server_is_registered_for_another_project() {
    let dir = TestDir::new("other-project");
    let registry_dir = dir.path().join("mcp-registry");
    let project = indexed_project(&dir, "proj");
    let other = dir.path().join("other-proj");
    let pid = std::process::id();
    seed_entry(&registry_dir, pid, Some(other.to_str().unwrap()));

    let run = run_index(&registry_dir, &project, &[]);

    assert!(
        run.success,
        "the warning must NOT change the exit code: stdout={} stderr={}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains(WARNING_MARKER),
        "a server launched for another project can still be asked to open this one, so it must \
         be warned about: stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.contains(&pid.to_string()),
        "the warning must name the holder's pid ({pid}): stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.contains("other-proj"),
        "each row must name the project the server was launched for, so a reader can tell the \
         rows apart: stderr={}",
        run.stderr
    );
}

/// An EMPTY registry and an UNREADABLE one both leave the successful `index`
/// output byte-for-byte unchanged: the pre-warning is the only new emission and
/// it is gated on a NON-EMPTY live set, so with nothing to name it adds zero
/// bytes. An outage stays silent here too — it says nothing about a holder, and a
/// line on every `index` run would be noise; it is surfaced on the FAILURE path
/// instead, where it is actionable.
///
/// (Before the launch-project field was demoted to a mere default, this case also
/// compared against a registry holding an unrelated project's server. That is no
/// longer a no-holder scenario — see
/// `warns_when_a_server_is_registered_for_another_project`.)
#[test]
fn empty_and_unreadable_registries_leave_the_success_path_unchanged() {
    let dir = TestDir::new("no-holder");
    let project = indexed_project(&dir, "proj");

    let empty_dir = dir.path().join("registry-empty");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let baseline = run_index(&empty_dir, &project, &[]);
    assert!(
        baseline.success,
        "baseline index must succeed: stderr={}",
        baseline.stderr
    );
    for marker in GUIDANCE_VOCABULARY {
        assert!(
            !baseline.stderr.contains(marker) && !baseline.stdout.contains(marker),
            "an empty registry must contribute ZERO bytes, but {marker:?} appeared: \
             stdout={} stderr={}",
            baseline.stdout,
            baseline.stderr
        );
    }

    let unreadable_dir = dir.path().join("registry-unreadable");
    std::fs::write(&unreadable_dir, b"not a directory").unwrap();
    let outage = run_index(&unreadable_dir, &project, &[]);
    assert!(
        outage.success,
        "an unreadable registry must not fail the index: stderr={}",
        outage.stderr
    );
    assert_eq!(
        normalize(&outage.stderr),
        normalize(&baseline.stderr),
        "stderr must be unchanged when there is no holder to report"
    );
    assert_eq!(
        normalize(&outage.stdout),
        normalize(&baseline.stdout),
        "stdout must be unchanged when there is no holder to report"
    );
}
