//! The CLI direct stdio serve path (`serve --mcp`) routes through the
//! rmcp `CodeGraphHandler` (the sole MCP transport).
//!
//! Drives the real `codegraph` binary end-to-end from an INDEXED cwd with the
//! daemon disabled (`CODEGRAPH_NO_DAEMON=1`, forcing `serve_direct`), sends an
//! `initialize` + a `tools/call codegraph_search` over stdio, and asserts a
//! non-empty, non-error tool result — proving the rmcp direct path serves the
//! tools. Then closes stdin and confirms the process exits (stdin EOF → rmcp
//! serve ends → block_on returns → exit).
//!
//! A second case reuses the same harness to cover the PID-keyed MCP registry: a
//! LIVE `serve --mcp` publishes a `<pid>.json` entry describing itself, and
//! stdin EOF removes it.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
            "codegraph-rmcp-serve-{label}-{}-{}",
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

fn indexed_project(dir: &TestDir) -> PathBuf {
    indexed_project_with(dir, "mini", None)
}

/// Copy the mini fixture to `<dir>/<name>` and index it. `extra` writes one more
/// source file BEFORE indexing, which is how a second project gets a symbol the
/// first one provably does not have.
fn indexed_project_with(dir: &TestDir, name: &str, extra: Option<(&str, &str)>) -> PathBuf {
    let project = dir.path().join(name);
    copy_tree(&mini_fixture(), &project);
    if let Some((relative, contents)) = extra {
        let file = project.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, contents).unwrap();
    }
    let status = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    project
}

fn read_json_line_with_id(
    reader: &mut impl BufRead,
    want_id: i64,
    deadline: Instant,
) -> Option<Value> {
    loop {
        if Instant::now() > deadline {
            return None;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(trimmed)
                    && value.get("id").and_then(Value::as_i64) == Some(want_id)
                {
                    return Some(value);
                }
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn serve_mcp_direct_routes_through_rmcp_handler() {
    // GIVEN an indexed project served directly (daemon disabled) with the rmcp
    // path opted in.
    let home = TestDir::new("indexed");
    let indexed = indexed_project(&home);

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", indexed.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp (rmcp)");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(20);

    // WHEN initialize + a tools/call are sent over stdio.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-serve-test", "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();

    let init_resp = read_json_line_with_id(&mut stdout, 1, deadline).expect("initialize result");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        json!("codegraph"),
        "the rmcp handler must identify as codegraph"
    );

    // The MCP spec requires the `initialized` notification after initialize.
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    writeln!(stdin, "{initialized}").unwrap();
    stdin.flush().unwrap();

    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": { "query": "add" } }
    });
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();

    let call_resp = read_json_line_with_id(&mut stdout, 2, deadline).expect("tools/call response");

    // THEN the tool call resolves against the pinned indexed project — non-empty
    // and not an error.
    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert_ne!(
        call_resp["result"]["isError"],
        json!(true),
        "rmcp direct-serve tool call must not error: {text}"
    );
    assert!(
        text.contains("add"),
        "rmcp direct-serve search must return results for the pinned index: {text}"
    );

    // Close stdin → EOF → rmcp serve ends → process exits.
    drop(stdin);
    let exited = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(
        exited,
        "serve --mcp (rmcp) must exit after stdin EOF (rmcp serve loop must end on stream close)"
    );
}

#[test]
fn serve_mcp_direct_registers_its_pid_and_unregisters_on_stdin_eof() {
    // GIVEN an indexed project served directly, with the PID-keyed MCP registry
    // pointed at an isolated temp dir (never the developer's real state dir).
    let home = TestDir::new("registry");
    let indexed = indexed_project(&home);
    let registry_dir = home.path().join("mcp-registry");

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", indexed.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp (rmcp)");

    let pid = child.id();
    let entry_path = registry_dir.join(format!("{pid}.json"));
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(20);

    // WHEN the server has answered `initialize` — proof it is past startup, so
    // registration (which happens before the serve loop) has already landed.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-registry-test", "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let init_resp = read_json_line_with_id(&mut stdout, 1, deadline).expect("initialize result");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        json!("codegraph")
    );

    // THEN a registry entry keyed by the live process's own pid describes it.
    let raw = std::fs::read_to_string(&entry_path).unwrap_or_else(|error| {
        panic!(
            "a live `serve --mcp` must register {}: {error}",
            entry_path.display()
        )
    });
    let entry: Value = serde_json::from_str(&raw).expect("registry entry is valid JSON");
    assert_eq!(
        entry["pid"],
        json!(pid),
        "the entry must carry the serving process's own pid: {raw}"
    );
    assert_eq!(entry["transport"], json!("stdio"), "{raw}");
    let project = entry["project"].as_str().unwrap_or_default();
    assert!(
        project.ends_with("mini"),
        "the entry must record the resolved project path: {raw}"
    );
    assert!(
        entry["version"].as_str().is_some_and(|v| !v.is_empty()),
        "the entry must record the binary version: {raw}"
    );
    assert!(
        entry["startedAt"].as_u64().is_some_and(|ms| ms > 0),
        "startedAt is camelCase epoch millis: {raw}"
    );

    // AND closing stdin (EOF → serve ends → process exits) unregisters it.
    drop(stdin);
    assert!(
        wait_with_timeout(&mut child, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
    assert!(
        !entry_path.exists(),
        "the registry entry must be removed on graceful shutdown: {}",
        entry_path.display()
    );
}

/// Drive `initialize` + the spec-required `initialized` notification, so a test
/// can go straight to `tools/call`. Answering `initialize` also proves the server
/// is past startup, hence past registry registration.
fn handshake(stdin: &mut impl Write, stdout: &mut impl BufRead, deadline: Instant, client: &str) {
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": client, "version": "0" }
        }
    });
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let resp = read_json_line_with_id(stdout, 1, deadline).expect("initialize result");
    assert_eq!(
        resp["result"]["serverInfo"]["name"],
        json!("codegraph"),
        "the rmcp handler must identify as codegraph"
    );
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    writeln!(stdin, "{initialized}").unwrap();
    stdin.flush().unwrap();
}

fn search(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    deadline: Instant,
    id: i64,
    arguments: Value,
) -> String {
    let resp = search_response(stdin, stdout, deadline, id, arguments);
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn search_response(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    deadline: Instant,
    id: i64,
    arguments: Value,
) -> Value {
    let call = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": "codegraph_search", "arguments": arguments }
    });
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();
    read_json_line_with_id(stdout, id, deadline).expect("tools/call response")
}

/// A `--path`-pinned server WILL open a DIFFERENT project's index when a client
/// passes an absolute `projectPath` — so the launch path is a DEFAULT, not an
/// access boundary. `resolve_project_arg` (`codegraph-mcp/src/roots.rs`) pushes an
/// absolute per-call path straight into its candidate list and probes it on its
/// own merits; the launch default is consulted only when no path was passed.
///
/// This is the premise the registry's `project` field must not be read as a
/// capability boundary — which is why `index`'s pre-warning reports every live
/// server instead of narrowing by that field. The existing
/// `resolve_project_arg_absolute_indexed_resolves` unit test does NOT cover it: it
/// builds the handler with `default_project = None`, so it never shows an absolute
/// argument WINNING OVER a pinned default. This case drives the real binary
/// end-to-end instead, and proves both directions at once.
#[test]
fn serve_mcp_resolves_an_absolute_project_path_outside_its_launch_path() {
    // GIVEN two indexed projects where only the second defines `betamarker`, and a
    // server pinned with `--path` to the first.
    let home = TestDir::new("cross-project");
    let pinned = indexed_project_with(&home, "mini-pinned", None);
    let other = indexed_project_with(
        &home,
        "mini-other",
        Some((
            "src/beta.ts",
            "export function betamarker(): number {\n  return 7;\n}\n",
        )),
    );

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", pinned.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp (rmcp)");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);
    handshake(&mut stdin, &mut stdout, deadline, "rmcp-cross-project-test");

    // WHEN the pinned default answers, THEN the other project's symbol is absent —
    // proving the two indexes are distinguishable.
    let default_hit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        2,
        json!({ "query": "betamarker" }),
    );
    assert!(
        default_hit.contains("No results found"),
        "the pinned project must not contain the other project's symbol: {default_hit}"
    );

    // WHEN an absolute `projectPath` names the OTHER project, THEN the server
    // opens THAT index despite having been launched with `--path` elsewhere.
    let cross_hit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        3,
        json!({ "query": "betamarker", "projectPath": other.to_str().unwrap() }),
    );
    assert!(
        !cross_hit.contains("No results found") && cross_hit.contains("beta.ts"),
        "a `--path`-pinned server must still resolve an absolute projectPath to another indexed \
         project — the launch path is a default, not a boundary: {cross_hit}"
    );

    drop(stdin);
    assert!(
        wait_with_timeout(&mut child, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
}

/// A BARE `serve --mcp` (no `--path`) records NO project. Its cwd is merely where
/// it happened to start: with no path pinned it resolves a project per request, so
/// recording cwd as "the project" would overstate what the entry knows. `mcp list`
/// renders the absent field as `<none>`.
#[test]
fn serve_mcp_without_an_explicit_path_registers_without_a_project() {
    // GIVEN a bare `serve --mcp` started INSIDE an indexed project.
    let home = TestDir::new("bare-launch");
    let indexed = indexed_project(&home);
    let registry_dir = home.path().join("mcp-registry");

    let mut child = Command::new(bin())
        .args(["serve", "--mcp"])
        .current_dir(&indexed)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bare serve --mcp");

    let pid = child.id();
    let entry_path = registry_dir.join(format!("{pid}.json"));
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);
    handshake(&mut stdin, &mut stdout, deadline, "rmcp-bare-launch-test");

    // THEN the entry exists but carries no project.
    let raw = std::fs::read_to_string(&entry_path).unwrap_or_else(|error| {
        panic!(
            "a live bare `serve --mcp` must still register {}: {error}",
            entry_path.display()
        )
    });
    let entry: Value = serde_json::from_str(&raw).expect("registry entry is valid JSON");
    assert_eq!(entry["pid"], json!(pid), "{raw}");
    assert!(
        entry.get("project").is_none(),
        "a bare `serve --mcp` has no pinned project, so the field must be absent rather than \
         recording the cwd it happened to start in: {raw}"
    );

    // AND it still serves the cwd project — the demoted field is informational.
    let hit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        2,
        json!({ "query": "add" }),
    );
    assert!(
        hit.contains("add"),
        "a bare launch must still resolve its cwd project per request: {hit}"
    );

    drop(stdin);
    assert!(
        wait_with_timeout(&mut child, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
}

#[test]
fn bare_serve_adopts_the_only_indexed_workspace_child() {
    // GIVEN an unindexed workspace root with exactly one indexed child.
    let workspace = TestDir::new("single-subproject");
    std::fs::create_dir(workspace.path().join(".git")).unwrap();
    let indexed = indexed_project_with(
        &workspace,
        "service-a",
        Some((
            "src/unique.ts",
            "export function uniquechildmarker(): number {\n  return 11;\n}\n",
        )),
    );
    let registry_dir = workspace.path().join("mcp-registry");

    let mut process = Command::new(bin())
        .args(["serve", "--mcp"])
        .current_dir(workspace.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bare serve --mcp from workspace root");

    let mut stdin = process.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(process.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);
    handshake(
        &mut stdin,
        &mut stdout,
        deadline,
        "rmcp-single-subproject-test",
    );

    // WHEN the client omits projectPath, THEN the unique child is the default.
    let hit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        2,
        json!({ "query": "uniquechildmarker" }),
    );
    assert!(
        hit.contains("unique.ts") && hit.contains("uniquechildmarker"),
        "the only indexed child must be adopted as the default project: {hit}"
    );

    drop(stdin);
    assert!(
        wait_with_timeout(&mut process, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
    let mut diagnostics = String::new();
    process
        .stderr
        .take()
        .expect("child stderr")
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(
        diagnostics.contains("adopted the single indexed sub-project service-a"),
        "startup must explain the deterministic adoption: {diagnostics}"
    );
    assert!(indexed.join(".codegraph/codegraph.db").is_file());
}

#[test]
fn bare_serve_reports_ambiguous_children_and_explicit_project_path_wins() {
    // GIVEN an unindexed workspace root with two indexed children.
    let workspace = TestDir::new("ambiguous-subprojects");
    std::fs::create_dir(workspace.path().join(".git")).unwrap();
    let service_a = indexed_project_with(
        &workspace,
        "service-a",
        Some((
            "src/alpha.ts",
            "export function alphachildmarker(): number {\n  return 1;\n}\n",
        )),
    );
    let service_b = indexed_project_with(
        &workspace,
        "service-b",
        Some((
            "src/beta.ts",
            "export function betachildmarker(): number {\n  return 2;\n}\n",
        )),
    );
    let registry_dir = workspace.path().join("mcp-registry");

    let mut process = Command::new(bin())
        .args(["serve", "--mcp"])
        .current_dir(workspace.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ambiguous bare serve --mcp");

    let mut stdin = process.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(process.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);
    handshake(
        &mut stdin,
        &mut stdout,
        deadline,
        "rmcp-ambiguous-subproject-test",
    );

    // No projectPath means no guessing; the existing tool-result error shape
    // lists candidates in deterministic order.
    let ambiguous_response = search_response(
        &mut stdin,
        &mut stdout,
        deadline,
        2,
        json!({ "query": "add" }),
    );
    assert!(
        ambiguous_response["result"]["isError"] != json!(true),
        "ambiguous defaults must use the existing success-shaped guidance result: {ambiguous_response}"
    );
    let ambiguous = ambiguous_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        ambiguous.contains("service-a, service-b") && ambiguous.contains("projectPath"),
        "ambiguous children must be reported rather than guessed: {ambiguous}"
    );

    // An explicit absolute projectPath bypasses the default-root ambiguity.
    let explicit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        3,
        json!({
            "query": "betachildmarker",
            "projectPath": service_b.to_str().unwrap()
        }),
    );
    assert!(
        explicit.contains("beta.ts") && explicit.contains("betachildmarker"),
        "explicit projectPath must win over ambiguous defaults: {explicit}"
    );
    assert!(service_a.join(".codegraph/codegraph.db").is_file());

    drop(stdin);
    assert!(
        wait_with_timeout(&mut process, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
    let mut diagnostics = String::new();
    process
        .stderr
        .take()
        .expect("child stderr")
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(
        diagnostics.contains("Indexed sub-projects found: service-a, service-b"),
        "stderr must explain the ambiguity and sorted choices: {diagnostics}"
    );
}

#[test]
fn bare_serve_does_not_scan_children_without_a_workspace_gate() {
    // GIVEN an ordinary unindexed directory with one indexed child but no
    // workspace manifest and no .git entry.
    let directory = TestDir::new("ungated-subproject");
    let indexed = indexed_project_with(
        &directory,
        "hidden-child",
        Some((
            "src/hidden.ts",
            "export function ungatedchildmarker(): number {\n  return 3;\n}\n",
        )),
    );
    let registry_dir = directory.path().join("mcp-registry");

    let mut process = Command::new(bin())
        .args(["serve", "--mcp"])
        .current_dir(directory.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_MCP_REGISTRY_DIR", &registry_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ungated bare serve --mcp");

    let mut stdin = process.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(process.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);
    handshake(
        &mut stdin,
        &mut stdout,
        deadline,
        "rmcp-ungated-subproject-test",
    );

    let no_default = search_response(
        &mut stdin,
        &mut stdout,
        deadline,
        2,
        json!({ "query": "ungatedchildmarker" }),
    );
    assert_ne!(no_default["result"]["isError"], json!(true), "{no_default}");
    let guidance = no_default["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        guidance.contains("No indexed project resolved") && !guidance.contains("hidden-child"),
        "an ungated directory must not down-scan its child: {guidance}"
    );

    let explicit = search(
        &mut stdin,
        &mut stdout,
        deadline,
        3,
        json!({
            "query": "ungatedchildmarker",
            "projectPath": indexed.to_str().unwrap()
        }),
    );
    assert!(
        explicit.contains("hidden.ts") && explicit.contains("ungatedchildmarker"),
        "the same child remains reachable when explicitly selected: {explicit}"
    );

    drop(stdin);
    assert!(
        wait_with_timeout(&mut process, Duration::from_secs(10)),
        "serve --mcp must exit after stdin EOF"
    );
    let mut diagnostics = String::new();
    process
        .stderr
        .take()
        .expect("child stderr")
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(
        diagnostics.contains("no default project, live sync disabled")
            && !diagnostics.contains("Indexed sub-projects found"),
        "stderr must report the no-default state without claiming an ungated scan: {diagnostics}"
    );
}

/// Poll for process exit within `timeout`, returning whether it exited (killing
/// it on timeout so the test process never leaks a child).
fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}
