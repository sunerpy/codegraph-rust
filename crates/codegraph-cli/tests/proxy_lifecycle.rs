//! A `serve --mcp` session proxied to a running daemon answers as a direct one.
//!
//! MCP 2026-07-28 (Claude Code 2.1.294 and later) has no `initialize`: a session
//! opens with `server/discover` and names its protocol version in every
//! request's `_meta`, and a result there carries `resultType: "complete"`. The
//! proxy used to answer every `tools/list` itself without it, and the client
//! refused the tools, but only in the second and later sessions of a project,
//! the ones a running daemon serves. Its local `initialize` answer also pinned
//! `2024-11-05` where the direct server echoes a version it can handshake at.
//!
//! Each case drives the same requests through a proxied session (a warm
//! `serve --mcp` while the project's daemon runs) and a direct one
//! (`CODEGRAPH_NO_DAEMON=1`, in a copy of the project so it does not contend
//! with the daemon for the writer slot), and compares every reply but its `id`.

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
            "codegraph-proxy-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        Self { base }
    }

    /// The registries a session publishes in, kept out of the user's.
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

    fn indexed_project(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.ts"),
            "export function alpha(): number { return beta(); }\n\
             export function beta(): number { return 1; }\n",
        )
        .unwrap();
        let init = self
            .command(&root)
            .args(["init", "."])
            .env("CODEGRAPH_NO_DAEMON", "1")
            .output()
            .expect("run codegraph init");
        assert!(init.status.success(), "{init:?}");
        root
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
    fn start(mut command: Command, stderr: &Path) -> Self {
        let mut child = command
            .args(["serve", "--mcp"])
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

/// The project's daemon, started the way a first session starts it, and kept
/// alive by that session until the test ends.
struct Daemon {
    keeper: Option<Session>,
    root: PathBuf,
}

impl Daemon {
    fn start(workspace: &Workspace, root: &Path) -> Self {
        let mut keeper =
            Session::start(workspace.command(root), &workspace.base.join("keeper.log"));
        keeper.send(&legacy("2024-11-05")[0]);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let up = codegraph_daemon::recorded_socket_path(root)
                .is_ok_and(|socket| codegraph_daemon::rendezvous_accepts(&socket));
            if up {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the project's daemon never came up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            keeper: Some(keeper),
            root: root.to_path_buf(),
        }
    }

    fn pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.root.join(".codegraph/daemon.pid")).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value.get("pid")?.as_u64().map(|pid| pid as u32)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(keeper) = self.keeper.take() {
            keeper.finish();
        }
        if let Some(pid) = self.pid() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && codegraph_daemon::is_process_alive(pid) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if codegraph_daemon::is_process_alive(pid) {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            }
        }
        codegraph_daemon::unlock_project(&self.root);
    }
}

/// Drive `script` through a proxied session on `proxied_root` (whose daemon
/// runs) and a direct one on `direct_root`, and return both reply lists.
fn both(
    workspace: &Workspace,
    proxied_root: &Path,
    direct_root: &Path,
    label: &str,
    script: &[Value],
) -> (Vec<Value>, Vec<Value>) {
    let proxied_log = workspace.base.join(format!("proxied-{label}.log"));
    let mut proxied_command = workspace.command(proxied_root);
    proxied_command.env("RUST_LOG", "debug");
    let proxied = replies(Session::start(proxied_command, &proxied_log), script);
    let log = std::fs::read_to_string(&proxied_log).unwrap();
    assert!(
        log.contains("attaching to existing daemon (warm)"),
        "the {label} session was not proxied:\n{log}"
    );
    let mut direct_command = workspace.command(direct_root);
    direct_command
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1");
    let direct_log = workspace.base.join(format!("direct-{label}.log"));
    let direct = replies(Session::start(direct_command, &direct_log), script);
    (proxied, direct)
}

#[test]
fn a_proxied_session_answers_both_lifecycles_as_a_direct_one() {
    let workspace = Workspace::new();
    let proxied_root = workspace.indexed_project("proxied");
    let direct_root = workspace.indexed_project("direct");
    let _daemon = Daemon::start(&workspace, &proxied_root);

    // 2026-07-28: discover, then tools/list, each naming its version in _meta.
    let modern = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta()}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": meta()}}),
    ];
    let (proxied, direct) = both(&workspace, &proxied_root, &direct_root, "2026", &modern);
    assert_eq!(proxied, direct);
    assert_eq!(
        proxied[1]["result"]["resultType"], "complete",
        "{:#}",
        proxied[1]
    );
    assert!(!proxied[1]["result"]["tools"].as_array().unwrap().is_empty());

    // The initialize lifecycle, asking for each kind of version.
    for (asked, negotiated) in [
        ("2025-06-18", "2025-06-18"),
        ("2024-11-05", "2024-11-05"),
        ("2026-07-28", "2024-11-05"),
        ("1999-01-01", "2024-11-05"),
    ] {
        let (proxied, direct) = both(
            &workspace,
            &proxied_root,
            &direct_root,
            &format!("initialize-{asked}"),
            &legacy(asked),
        );
        assert_eq!(proxied, direct, "initialize asking for {asked}");
        assert_eq!(
            proxied[0]["result"]["protocolVersion"], negotiated,
            "{asked}"
        );
        assert!(proxied[1]["result"].get("resultType").is_none(), "{asked}");
    }
}
