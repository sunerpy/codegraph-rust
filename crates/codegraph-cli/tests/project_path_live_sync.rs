//! End-to-end acceptance for upstream #1835: an unpinned MCP session may access
//! several existing indexes through explicit `projectPath` values. First access
//! waits for catch-up, each project gets one shared daemon/watcher owner, and a
//! second connected MCP session keeps that owner alive after the first closes.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-project-path-live-{label}-{}-{}",
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

fn initialized_child(workspace: &Path, name: &str) -> PathBuf {
    let project = workspace.join(name);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir(project.join(".git")).unwrap();
    fs::write(
        project.join("src/live.ts"),
        format!("export function initial_{name}(): number {{ return 1; }}\n"),
    )
    .unwrap();
    let output = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .output()
        .expect("run codegraph init");
    assert!(
        output.status.success(),
        "init {} failed: stdout={} stderr={}",
        project.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    project
}

struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpClient {
    fn spawn(workspace: &Path, registry: &Path, name: &str) -> Self {
        let mut child = Command::new(bin())
            .args(["serve", "--mcp"])
            .current_dir(workspace)
            .env("CODEGRAPH_MCP_REGISTRY_DIR", registry)
            .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "60000")
            .env("CODEGRAPH_DAEMON_MAX_IDLE_MS", "60000")
            .env("CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS", "60000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn global MCP client process");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));
        let mut client = Self {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 1,
        };
        let init_id = client.next_id();
        client.write(json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": name, "version": "0" }
            }
        }));
        let response = client.read_id(init_id);
        assert_eq!(response["result"]["serverInfo"]["name"], "codegraph");
        client.write(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
        client
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().expect("MCP stdin remains open");
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }

    fn read_id(&mut self, want: i64) -> Value {
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read MCP response");
            assert!(read > 0, "MCP process closed before response id {want}");
            if let Ok(value) = serde_json::from_str::<Value>(line.trim())
                && value.get("id").and_then(Value::as_i64) == Some(want)
            {
                return value;
            }
        }
    }

    fn search(&mut self, project: &Path, query: &str) -> String {
        let id = self.next_id();
        self.write(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "codegraph_search",
                "arguments": {
                    "query": query,
                    "projectPath": project
                }
            }
        }));
        let response = self.read_id(id);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert_ne!(
            response["result"]["isError"],
            json!(true),
            "explicit project search failed: {text}"
        );
        text
    }

    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("MCP process did not exit after stdin EOF");
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn daemon_pid(project: &Path) -> Option<u32> {
    let path = codegraph_daemon::daemon_pid_path(project).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    codegraph_daemon::decode_lock_info(&raw).map(|info| info.pid)
}

fn wait_for_daemon_pid(project: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = daemon_pid(project)
            && codegraph_daemon::is_process_alive(pid)
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not start for {}",
            project.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn is_symbol_hit(text: &str, symbol: &str) -> bool {
    text.contains(symbol) && text.contains("src/live.ts") && !text.contains("No results found")
}

fn wait_for_watched_symbol(client: &mut McpClient, project: &Path, symbol: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let text = client.search(project, symbol);
        if is_symbol_hit(&text, symbol) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not index {symbol} in {}: {text}",
            project.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn shutdown_daemon(project: &Path) {
    let paths = codegraph_core::IndexPaths::resolve(project, None).unwrap();
    match codegraph_daemon::request_daemon_shutdown(project, paths.project_identity()) {
        Ok(codegraph_daemon::ShutdownOutcome::NoDaemon)
        | Ok(codegraph_daemon::ShutdownOutcome::Drained { .. }) => {}
        other => panic!("failed to stop daemon for {}: {other:?}", project.display()),
    }
}

#[test]
fn explicit_projects_catch_up_and_share_watcher_ownership_across_sessions() {
    let workspace = TestDir::new("workspace");
    fs::create_dir(workspace.path().join(".git")).unwrap();
    let service_a = initialized_child(workspace.path(), "service-a");
    let service_b = initialized_child(workspace.path(), "service-b");
    let registry = workspace.path().join("mcp-registry");

    let mut first = McpClient::spawn(workspace.path(), &registry, "project-path-first");
    let mut second = McpClient::spawn(workspace.path(), &registry, "project-path-second");

    // The edit happens after initialization but before the first explicit access.
    // The first response must therefore wait for catch-up rather than serve the
    // stale index that was present when the MCP process started.
    let catchup_a = "firstExplicitCatchupUniqueA";
    fs::write(
        service_a.join("src/live.ts"),
        format!("export function {catchup_a}(): number {{ return 10; }}\n"),
    )
    .unwrap();
    let first_result = first.search(&service_a, catchup_a);
    assert!(is_symbol_hit(&first_result, catchup_a), "{first_result}");
    let first_pid = wait_for_daemon_pid(&service_a);

    // A second MCP session attaches the same repository service. It must share
    // the existing daemon/writer rather than launching a second watcher owner.
    let second_result = second.search(&service_a, catchup_a);
    assert!(is_symbol_hit(&second_result, catchup_a), "{second_result}");
    assert_eq!(wait_for_daemon_pid(&service_a), first_pid);

    // Subsequent edits are delivered by the retained daemon watcher. Repeated
    // tool calls do not re-run catch-up because this session already holds A.
    let watched_a = "subsequentWatcherUpdateUniqueA";
    fs::write(
        service_a.join("src/live.ts"),
        format!("export function {watched_a}(): number {{ return 200; }}\n"),
    )
    .unwrap();
    wait_for_watched_symbol(&mut second, &service_a, watched_a);

    // Closing one MCP session drops only its daemon connection. The second
    // session's retained connection keeps the same watcher owner alive.
    first.close();
    assert!(codegraph_daemon::is_process_alive(first_pid));
    let after_close_a = "watchSurvivesOtherSessionCloseUniqueA";
    fs::write(
        service_a.join("src/live.ts"),
        format!("export function {after_close_a}(): number {{ return 3000; }}\n"),
    )
    .unwrap();
    wait_for_watched_symbol(&mut second, &service_a, after_close_a);
    assert_eq!(wait_for_daemon_pid(&service_a), first_pid);

    // A different explicit repository remains separate: it gets its own index,
    // catch-up, and daemon, without becoming an arbitrary default.
    let catchup_b = "firstExplicitCatchupUniqueB";
    fs::write(
        service_b.join("src/live.ts"),
        format!("export function {catchup_b}(): number {{ return 40; }}\n"),
    )
    .unwrap();
    let b_result = second.search(&service_b, catchup_b);
    assert!(is_symbol_hit(&b_result, catchup_b), "{b_result}");
    let second_pid = wait_for_daemon_pid(&service_b);
    assert_ne!(
        second_pid, first_pid,
        "each repository owns a separate daemon"
    );

    second.close();
    shutdown_daemon(&service_a);
    shutdown_daemon(&service_b);
}
