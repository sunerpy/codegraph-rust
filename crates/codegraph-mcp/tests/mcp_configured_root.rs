//! Real MCP public-surface regressions for a configured `CODEGRAPH_DIR`.
//!
//! Isolated in its OWN test binary (a separate process) because these tests set
//! the process-global `CODEGRAPH_DIR`; keeping them out of `golden_mcp.rs` avoids
//! racing that binary's env-sensitive default-surface readers. Each test drives
//! the REAL `McpServer` over an in-memory stdio pipe and proves the MCP request
//! path opens the SAME identity-suffixed DB the CLI `init` writes.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_mcp::McpServer;
use serde_json::{Value, json};

static SEQ: AtomicU64 = AtomicU64::new(0);
// Both tests mutate the PROCESS-GLOBAL `CODEGRAPH_DIR`; cargo runs a binary's
// tests multi-threaded in ONE process, so they must serialize the set→read→
// restore window against each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

struct TempDir {
    path: PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique_dir(tag: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "cg-mcp-cfgroot-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    TempDir { path }
}

fn stage_mini_db_at(db: &Path, project: &Path) {
    let root = workspace_root();
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::copy(root.join("reference/golden/mini/colby.db"), db).unwrap();
    let fixtures = root.join("crates/codegraph-bench/fixtures/mini");
    for rel in ["src/app.ts", "src/math.ts"] {
        let dst = project.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(fixtures.join(rel), &dst).unwrap();
    }
}

fn tool_call_search(project: &Path, project_path_arg: &str) -> Value {
    let request = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "codegraph_search",
            "arguments": { "query": "add", "projectPath": project_path_arg }
        }
    });
    let frame = format!("{}\n", serde_json::to_string(&request).unwrap());
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(project.to_path_buf()));
    server
        .run_until_adoption(Cursor::new(frame.into_bytes()), &mut output)
        .expect("server run");
    let text = String::from_utf8(output).expect("utf8");
    let line = text.lines().next().expect("one response line");
    serde_json::from_str(line).expect("response json")
}

/// A relative `CODEGRAPH_DIR` is honored through `resolve`'s identity-suffixed
/// sibling: staging the golden DB at `<parent>/<name>-v2-<identity>/codegraph.db`
/// (NOT the simple-join `<project>/<name>`) makes a real `tools/call` resolve and
/// return results — proving the MCP path opens the DB the CLI init would write.
#[test]
fn configured_relative_root_mcp_opens_identity_sibling_db() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = unique_dir("rel");
    let base = &dir.path;

    let prev = std::env::var_os("CODEGRAPH_DIR");
    // SAFETY: this test binary runs single-threaded per test process; the env is
    // set before any resolve/DB access and restored before returning.
    unsafe { std::env::set_var("CODEGRAPH_DIR", "cache") };

    let paths = codegraph_core::IndexPaths::resolve(base, Some("cache")).expect("resolve");
    stage_mini_db_at(&paths.current_db(), base);
    assert!(
        !base.join("cache").join("codegraph.db").is_file(),
        "configured root must resolve to the identity sibling, not the simple-join"
    );

    let resp = tool_call_search(base, base.to_str().unwrap());

    match prev {
        Some(v) => unsafe { std::env::set_var("CODEGRAPH_DIR", v) },
        None => unsafe { std::env::remove_var("CODEGRAPH_DIR") },
    }

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    assert_ne!(
        resp["result"]["isError"],
        json!(true),
        "configured-root tool call must not error: {text}"
    );
    assert!(
        text.contains("add"),
        "search against the identity-sibling DB must return results: {text}"
    );
}

/// Two DISTINCT projects sharing ONE absolute `CODEGRAPH_DIR` cannot cross-read:
/// each resolves to its OWN identity-suffixed sibling, so staging only project
/// A's DB leaves project B unindexed — B can never open A's graph through the
/// shared absolute root.
#[test]
fn two_projects_sharing_absolute_root_cannot_cross_read_via_mcp() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = unique_dir("abs");
    let holder = &dir.path;
    let project_a = holder.join("a");
    let project_b = holder.join("b");
    let shared = holder.join("shared").join("cg");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    fs::create_dir_all(holder.join("shared")).unwrap();

    let prev = std::env::var_os("CODEGRAPH_DIR");
    let shared_str = shared.to_string_lossy().into_owned();
    // SAFETY: single-threaded test process; restored before returning.
    unsafe { std::env::set_var("CODEGRAPH_DIR", &shared_str) };

    let paths_a = codegraph_core::IndexPaths::resolve(&project_a, Some(&shared_str)).expect("a");
    stage_mini_db_at(&paths_a.current_db(), &project_a);

    let paths_b = codegraph_core::IndexPaths::resolve(&project_b, Some(&shared_str)).expect("b");
    assert_ne!(
        paths_a.current_root(),
        paths_b.current_root(),
        "two projects sharing an absolute root must get distinct identity siblings"
    );
    assert!(
        !paths_b.current_db().is_file(),
        "project B must NOT resolve to project A's staged DB"
    );

    let resp_a = tool_call_search(&project_a, project_a.to_str().unwrap());
    let resp_b = tool_call_search(&project_b, project_b.to_str().unwrap());

    match prev {
        Some(v) => unsafe { std::env::set_var("CODEGRAPH_DIR", v) },
        None => unsafe { std::env::remove_var("CODEGRAPH_DIR") },
    }

    let text_a = resp_a["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        resp_a["result"]["isError"] != json!(true) && text_a.contains("add"),
        "project A must read its own graph: {text_a}"
    );
    let text_b = serde_json::to_string(&resp_b).unwrap();
    assert!(
        !text_b.contains("Greeter") && !text_b.contains("Counter"),
        "project B must NOT surface project A's indexed symbols: {text_b}"
    );
}
