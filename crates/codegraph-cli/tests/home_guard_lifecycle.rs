//! A `serve --mcp` session at `$HOME` answers both lifecycles as a direct one.
//!
//! At a too-broad root (`$HOME` or the filesystem root) the server starts no
//! daemon, watcher or catch-up, and serves through the hand-rolled loop that can
//! hand the session to a daemon once the client's roots name an indexed
//! project. That loop knew only the `initialize` lifecycle: MCP 2026-07-28's
//! `server/discover` got "Method not found", its `tools/list` came back without
//! `resultType: "complete"`, and an `initialize` was answered with `2024-11-05`
//! whatever version it asked for. A session that opens with the inline lifecycle
//! is now served by the rmcp handler the direct server uses.
//!
//! Each case drives the same requests through a session at a temporary `$HOME`
//! and a direct one (`CODEGRAPH_NO_DAEMON=1`) in an unindexed folder, and
//! compares every reply but its `id`.

#![cfg(unix)]

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const REPLY_DEADLINE: Duration = Duration::from_secs(30);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct Workspace {
    base: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "codegraph-home-guard-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for dir in ["home", "plain"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        Self { base }
    }

    fn home(&self) -> PathBuf {
        self.base.join("home")
    }

    /// The registries a session publishes in, outside `$HOME` and the user's.
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(bin());
        command
            .current_dir(cwd)
            .env("CODEGRAPH_MCP_REGISTRY_DIR", self.base.join("mcp-registry"))
            .env(
                "CODEGRAPH_HTTP_REGISTRY_DIR",
                self.base.join("http-registry"),
            )
            .env_remove("CODEGRAPH_NO_DAEMON")
            .env_remove("CODEGRAPH_NO_WATCH")
            .env_remove("RUST_LOG");
        command
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// One `serve --mcp` process: requests in, replies by id out.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: mpsc::Receiver<Value>,
}

impl Session {
    fn start(mut command: Command, args: &[&str], stderr: &Path) -> Self {
        let mut child = command
            .args(["serve", "--mcp"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(stderr).unwrap())
            .spawn()
            .expect("start serve --mcp");
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            replies,
        }
    }

    /// Send `request`; for a request with an id, wait for the reply to it.
    fn send(&mut self, request: &Value) -> Option<Value> {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        let id = request.get("id")?;
        let deadline = Instant::now() + REPLY_DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let reply = self
                .replies
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no reply to {request} within {REPLY_DEADLINE:?}"));
            if reply.get("id") == Some(id) {
                return Some(reply);
            }
        }
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every reply to `script`, without its `id`.
fn replies(mut session: Session, script: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for request in script {
        if let Some(mut reply) = session.send(request) {
            reply.as_object_mut().unwrap().remove("id");
            out.push(reply);
        }
    }
    session.finish();
    out
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "lifecycle-test", "version": "0"}
    })
}

fn legacy(version: &str) -> Vec<Value> {
    vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": {},
            "clientInfo": {"name": "lifecycle-test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]
}

/// Drive `script` through a session at the workspace's `$HOME` and a direct one
/// in its unindexed `plain` folder, and return both reply lists.
fn both(workspace: &Workspace, label: &str, script: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let home = workspace.home();
    let home_log = workspace.base.join(format!("home-{label}.log"));
    let mut home_command = workspace.command(&home);
    home_command.env("HOME", &home);
    let at_home = replies(
        Session::start(home_command, &["--path", home.to_str().unwrap()], &home_log),
        script,
    );
    let log = std::fs::read_to_string(&home_log).unwrap();
    assert!(
        log.contains("home directory"),
        "the {label} session did not take the too-broad-root branch:\n{log}"
    );
    let mut direct_command = workspace.command(&workspace.base.join("plain"));
    direct_command
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1");
    let direct_log = workspace.base.join(format!("direct-{label}.log"));
    let direct = replies(Session::start(direct_command, &[], &direct_log), script);
    (at_home, direct)
}

#[test]
fn a_session_at_home_answers_both_lifecycles_as_a_direct_one() {
    let workspace = Workspace::new();

    // 2026-07-28: discover, then tools/list, each naming its version in _meta.
    let modern = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta()}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": meta()}}),
    ];
    let (at_home, direct) = both(&workspace, "2026", &modern);
    assert_eq!(at_home, direct);
    assert!(at_home[0].get("result").is_some(), "{:#}", at_home[0]);
    assert_eq!(
        at_home[1]["result"]["resultType"], "complete",
        "{:#}",
        at_home[1]
    );
    assert!(!at_home[1]["result"]["tools"].as_array().unwrap().is_empty());

    // The initialize lifecycle, asking for each kind of version.
    for (asked, negotiated) in [
        ("2025-06-18", "2025-06-18"),
        ("2024-11-05", "2024-11-05"),
        ("2026-07-28", "2024-11-05"),
        ("1999-01-01", "2024-11-05"),
    ] {
        let (at_home, direct) = both(&workspace, &format!("initialize-{asked}"), &legacy(asked));
        assert_eq!(at_home, direct, "initialize asking for {asked}");
        assert_eq!(
            at_home[0]["result"]["protocolVersion"], negotiated,
            "{asked}"
        );
        assert!(at_home[1]["result"].get("resultType").is_none(), "{asked}");
    }
}
