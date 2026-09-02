//! End-to-end coverage for the opt-in JSONL diagnostics surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-debug-diagnostics-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn run(cwd: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .current_dir(cwd)
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", cwd.join("http-registry"))
        .output()
        .expect("run codegraph");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn parse_jsonl(path: &Path) -> (String, Vec<Value>) {
    let raw = fs::read_to_string(path).unwrap();
    let events = raw
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| {
        event["schemaVersion"] == 1
            && event["timestamp"].is_string()
            && event["elapsedMs"].is_number()
            && event["sessionId"].is_string()
            && event["event"].is_string()
    }));
    (raw, events)
}

fn only_log_with_prefix(project: &Path, prefix: &str) -> PathBuf {
    let mut matches = fs::read_dir(project.join(".codegraph/diagnostics"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".jsonl"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.pop().expect("diagnostic log")
}

#[test]
fn init_index_and_sync_debug_logs_are_parseable_private_and_discoverable() {
    const SECRET: &str = "SUPER_SECRET_SOURCE_MARKER_7fd19";

    let temp = TestDir::new();
    let project = temp.path.join("project");
    fs::create_dir_all(project.join("module-a/src")).unwrap();
    fs::create_dir_all(project.join("frontend/src")).unwrap();
    fs::create_dir_all(project.join("mobile/src")).unwrap();
    fs::write(
        project.join("module-a/src/App.java"),
        format!("class App {{ String secret = \"{SECRET}\"; }}\n"),
    )
    .unwrap();
    fs::write(
        project.join("frontend/src/App.vue"),
        "<template><Button /></template><script setup>const x = 1</script>\n",
    )
    .unwrap();
    fs::write(
        project.join("mobile/src/main.ts"),
        "export function boot() { return 1; }\n",
    )
    .unwrap();
    let project_arg = project.to_str().unwrap();

    let init = run(&temp.path, &["init", project_arg, "--debug"]);
    assert!(init.ok, "init failed: {} {}", init.stdout, init.stderr);
    assert_eq!(init.stderr.matches("Debug log:").count(), 1);
    let init_log = only_log_with_prefix(&project, "init-");
    let (raw, events) = parse_jsonl(&init_log);
    assert!(!raw.contains(SECRET), "source content leaked into JSONL");
    assert!(events.iter().any(|event| event["event"] == "file_complete"));
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "resolution_setup")
    );
    assert!(
        events
            .iter()
            .any(|event| { event["event"] == "session_end" && event["status"] == "success" })
    );

    let custom_log = temp.path.join("custom-index.jsonl");
    let custom_arg = custom_log.to_str().unwrap();
    let index = run(
        &temp.path,
        &[
            "index",
            "--force",
            "--quiet",
            "--debug-log",
            custom_arg,
            project_arg,
        ],
    );
    assert!(index.ok, "index failed: {} {}", index.stdout, index.stderr);
    assert!(index.stdout.is_empty(), "--quiet stdout: {}", index.stdout);
    assert_eq!(index.stderr.matches("Debug log:").count(), 1);
    assert!(!index.stderr.contains("Indexing ["));
    let (raw, events) = parse_jsonl(&custom_log);
    assert!(!raw.contains(SECRET), "source content leaked into JSONL");
    assert!(events.iter().any(|event| event["event"] == "file_complete"));

    fs::write(
        project.join("mobile/src/main.ts"),
        "export function boot() { return 2; }\n",
    )
    .unwrap();
    let sync = run(&temp.path, &["sync", "--quiet", "--debug", project_arg]);
    assert!(sync.ok, "sync failed: {} {}", sync.stdout, sync.stderr);
    assert!(sync.stdout.is_empty(), "--quiet stdout: {}", sync.stdout);
    assert_eq!(sync.stderr.matches("Debug log:").count(), 1);
    let sync_log = only_log_with_prefix(&project, "sync-");
    let (_raw, events) = parse_jsonl(&sync_log);
    assert!(events.iter().any(|event| {
        event["event"] == "file_complete" && event["file"] == "mobile/src/main.ts"
    }));
    assert!(
        events
            .iter()
            .any(|event| { event["event"] == "session_end" && event["status"] == "success" })
    );
}
