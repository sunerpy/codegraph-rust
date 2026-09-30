//! End-to-end writer ownership for long-lived direct MCP services (#1740).
//!
//! The unit tests pin lockfile mechanics. These tests drive the real binary so
//! CLI mode selection, RAII lifetime, and the no-implicit-index boundary cannot
//! drift independently of those mechanics.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-mcp-writer-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn initialized_project(label: &str) -> TestDir {
    let dir = TestDir::new(label);
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/a.ts"),
        "export function alpha(): number { return 1; }\n",
    )
    .unwrap();
    let output = Command::new(bin())
        .args(["init", dir.path().to_str().unwrap()])
        .output()
        .expect("run codegraph init");
    assert!(
        output.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

fn spawn_direct(project: &Path) -> Child {
    Command::new(bin())
        .args(["serve", "--mcp", "--path", project.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn direct MCP server")
}

fn initialize(child: &mut Child) {
    let stdin = child.stdin.as_mut().expect("child stdin remains open");
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"writer-lock-test","version":"0"}}}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().expect("poll child").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.try_wait().expect("final child poll").is_some()
}

#[test]
fn second_explicit_direct_writer_fails_fast_and_the_owner_cleans_up() {
    let project = initialized_project("exclusive");
    let writer_path = codegraph_daemon::writer_pid_path(project.path()).unwrap();
    let mut first = spawn_direct(project.path());
    initialize(&mut first);
    assert!(
        wait_for_path(&writer_path, Duration::from_secs(10)),
        "first direct server never published {}",
        writer_path.display()
    );
    assert!(
        first.try_wait().unwrap().is_none(),
        "first writer exited before contention"
    );

    let mut second = spawn_direct(project.path());
    assert!(
        wait_for_exit(&mut second, Duration::from_secs(10)),
        "second direct writer did not fail fast"
    );
    let status = second.wait().expect("collect second status");
    let mut stderr = String::new();
    second
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "second writer unexpectedly succeeded");
    assert!(
        stderr.contains("writer lock held") && stderr.contains("CODEGRAPH_NO_DAEMON"),
        "contention error must be actionable: {stderr}"
    );
    assert!(
        first.try_wait().unwrap().is_none(),
        "contending process must not terminate the established writer"
    );

    drop(first.stdin.take());
    if !wait_for_exit(&mut first, Duration::from_secs(10)) {
        let _ = first.kill();
        panic!("first direct server did not exit after stdin EOF");
    }
    assert!(first.wait().unwrap().success());
    assert!(
        writer_path.is_file(),
        "writer.pid is a stable kernel-lock path"
    );
    assert!(
        codegraph_daemon::read_writer_lock(project.path()).is_none(),
        "ordinary EOF shutdown must release the kernel lock and clear diagnostics"
    );
    assert_eq!(fs::metadata(writer_path).unwrap().len(), 0);
}

#[test]
fn explicit_unindexed_path_stays_state_free() {
    let project = TestDir::new("unindexed");
    fs::write(project.path().join("a.ts"), "export const a = 1;\n").unwrap();
    let index_root = project.path().join(".codegraph");
    let mut server = spawn_direct(project.path());
    initialize(&mut server);
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        server.try_wait().unwrap().is_none(),
        "unindexed explicit server should stay available for guidance/query errors"
    );
    assert!(
        !index_root.exists(),
        "serve --mcp --path must not create an index or writer namespace"
    );
    drop(server.stdin.take());
    if !wait_for_exit(&mut server, Duration::from_secs(10)) {
        let _ = server.kill();
        panic!("unindexed server did not exit after stdin EOF");
    }
    assert!(server.wait().unwrap().success());
}
