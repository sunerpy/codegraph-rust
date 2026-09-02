//! Opt-in synthetic benchmark for the reported 65-module Java + Vue + mobile
//! project shape. It avoids wall-clock assertions; the hard checks are bounded
//! scheduling, complete persistence, and a successful deterministic run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-large-multimodule-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
#[ignore = "large synthetic benchmark; run explicitly when changing index scheduling"]
fn sixty_five_java_modules_vue_and_mobile_stay_within_the_parse_window() {
    const JAVA_MODULES: usize = 65;
    const JAVA_FILES_PER_MODULE: usize = 80;
    const VUE_FILES: usize = 400;
    const MOBILE_FILES: usize = 400;
    const TOTAL_FILES: usize = JAVA_MODULES * JAVA_FILES_PER_MODULE + VUE_FILES + MOBILE_FILES;

    let temp = TestDir::new();
    let project = temp.0.join("project");
    for module in 0..JAVA_MODULES {
        for file in 0..JAVA_FILES_PER_MODULE {
            write_file(
                &project,
                &format!("module-{module:02}/src/main/java/p{module:02}/C{file:03}.java"),
                &format!(
                    "package p{module:02}; public class C{file:03} {{ public int id() {{ return {file}; }} }}\n"
                ),
            );
        }
    }
    for file in 0..VUE_FILES {
        write_file(
            &project,
            &format!("web/src/components/View{file:03}.vue"),
            &format!(
                "<template><div>View {file}</div></template><script setup lang=\"ts\">const id = {file}</script>\n"
            ),
        );
    }
    for file in 0..MOBILE_FILES {
        write_file(
            &project,
            &format!("mobile/src/screen{file:03}.ts"),
            &format!("export function screen{file:03}() {{ return {file}; }}\n"),
        );
    }
    assert_eq!(TOTAL_FILES, 6_000);

    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(["init", project.to_str().unwrap(), "--debug"])
        .env("RAYON_NUM_THREADS", "2")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run synthetic index");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_dir(project.join(".codegraph/diagnostics"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("init-"))
        })
        .expect("init diagnostic log");
    let events = fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "file_complete")
            .count(),
        TOTAL_FILES
    );
    for heartbeat in events.iter().filter(|event| event["event"] == "heartbeat") {
        let scheduled = heartbeat["scheduled"].as_u64().unwrap();
        let persisted = heartbeat["persisted"].as_u64().unwrap();
        assert!(
            scheduled - persisted <= 512,
            "parse window exceeded: {heartbeat}"
        );
    }
    assert!(
        !events.iter().any(|event| event["event"] == "slow_file"),
        "small synthetic files should not trigger a stalled parse"
    );
    assert!(
        events
            .iter()
            .any(|event| { event["event"] == "session_end" && event["status"] == "success" })
    );
}
