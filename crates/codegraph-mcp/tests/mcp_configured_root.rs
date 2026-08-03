//! Real MCP public-surface regressions for a configured `CODEGRAPH_DIR`.
//!
//! Isolated in its OWN test binary (a separate process) because these tests set
//! the process-global `CODEGRAPH_DIR`; keeping them out of `golden_mcp.rs` avoids
//! racing that binary's env-sensitive default-surface readers. The tests drive a
//! REAL server front-end — most over an in-memory stdio pipe against `McpServer`,
//! and one against the SHIPPED rmcp handler over a duplex transport — and prove
//! the MCP request path opens the SAME project-local DB the CLI `init` writes,
//! and fails closed identically on an invalid configured root.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_mcp::McpServer;
use serde_json::{Value, json};

static SEQ: AtomicU64 = AtomicU64::new(0);
// Every test here mutates the PROCESS-GLOBAL `CODEGRAPH_DIR`; cargo runs a
// binary's tests multi-threaded in ONE process, so they must serialize the
// set→read→restore window against each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard for the process-global `CODEGRAPH_DIR`: sets it on construction and
/// restores the PREVIOUS value (or removes it) in `Drop`. Manual restore lines
/// are skipped when an assertion panics mid-test, leaking a bad `CODEGRAPH_DIR`
/// into every later test in this binary; `Drop` runs on the unwind path too, so
/// the restoration is panic-safe. Holds the [`ENV_LOCK`] for its whole lifetime.
struct EnvGuard {
    prev: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(value: &str) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("CODEGRAPH_DIR");
        // SAFETY: the ENV_LOCK held for this guard's lifetime serializes every
        // `CODEGRAPH_DIR` mutation in this test binary, and `Drop` restores the
        // prior value on both the normal and the unwind path.
        unsafe { std::env::set_var("CODEGRAPH_DIR", value) };
        Self { prev, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: still holding the ENV_LOCK, so no other test thread is reading
        // or writing `CODEGRAPH_DIR` while it is restored.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var("CODEGRAPH_DIR", v) },
            None => unsafe { std::env::remove_var("CODEGRAPH_DIR") },
        }
    }
}

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

fn stage_mini_db_at(paths: &codegraph_core::IndexPaths, project: &Path) {
    let root = workspace_root();
    let db = paths.current_db();
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::copy(root.join("reference/golden/mini/colby.db"), &db).unwrap();
    let fixtures = root.join("crates/codegraph-bench/fixtures/mini");
    for rel in ["src/app.ts", "src/math.ts"] {
        let dst = project.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(fixtures.join(rel), &dst).unwrap();
    }
    codegraph_store::test_support::finalize_current_test_fixture(paths).unwrap();
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

#[test]
fn configured_relative_root_mcp_opens_project_local_db() {
    let dir = unique_dir("rel");
    let base = &dir.path;

    let _env = EnvGuard::set("cache");

    let paths = codegraph_core::IndexPaths::resolve(base, Some("cache")).expect("resolve");
    stage_mini_db_at(&paths, base);
    assert!(
        base.join("cache").join("codegraph.db").is_file(),
        "configured root must resolve to the direct project-local join"
    );

    let resp = tool_call_search(base, base.to_str().unwrap());

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
        "search against the configured project-local DB must return results: {text}"
    );
}

#[test]
fn absolute_configured_root_fails_closed_via_mcp() {
    let dir = unique_dir("abs");
    let holder = &dir.path;
    let project_a = holder.join("a");
    let project_b = holder.join("b");
    let shared = holder.join("shared").join("cg");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    fs::create_dir_all(holder.join("shared")).unwrap();

    let default_paths = codegraph_core::IndexPaths::resolve(&project_a, None).expect("default");
    stage_mini_db_at(&default_paths, &project_a);

    let shared_str = shared.to_string_lossy().into_owned();
    let _env = EnvGuard::set(&shared_str);

    assert!(
        codegraph_core::IndexPaths::resolve(&project_a, Some(&shared_str)).is_err(),
        "absolute CODEGRAPH_DIR must be rejected"
    );

    let resp_a = tool_call_search(&project_a, project_a.to_str().unwrap());
    let resp_b = tool_call_search(&project_b, project_b.to_str().unwrap());
    for response in [resp_a, resp_b] {
        let whole = serde_json::to_string(&response).unwrap();
        assert_eq!(response["result"]["isError"], json!(true), "{whole}");
        assert!(whole.contains("CODEGRAPH_DIR"), "{whole}");
        assert!(
            !whole.contains("Counter") && !whole.contains("Greeter"),
            "{whole}"
        );
    }
    assert!(
        !shared.exists(),
        "invalid external root must not be created"
    );
}

/// The exact payload of ONE filesystem entry in the nonmutation oracle: a
/// directory's mere presence (so an EMPTY directory's creation/removal is
/// detectable), a regular file's COMPLETE bytes (never its length — an
/// equal-length in-place write keeps the size identical), or a symlink's TARGET
/// (the link is never followed).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    Directory,
    RegularFile(Vec<u8>),
    Symlink(PathBuf),
}

/// One snapshot entry keyed by its OS-native relative path, so the equality key
/// is never a lossy `to_string_lossy` rendering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TreeEntry {
    rel: PathBuf,
    kind: EntryKind,
}

fn kind_label(kind: &EntryKind) -> String {
    match kind {
        EntryKind::Directory => "directory".to_string(),
        EntryKind::RegularFile(bytes) => format!("file[{} bytes]", bytes.len()),
        EntryKind::Symlink(target) => format!("symlink -> {}", target.display()),
    }
}

/// Snapshot EVERY filesystem entry under `root` — directories, regular files
/// (complete bytes) and symlinks (their targets) — sorted; the nonmutation
/// oracle, identical in semantics to the CLI-side one.
///
/// FAIL-CLOSED: every I/O step panics on error instead of skipping or
/// defaulting. A swallowed `read_dir`/entry error would drop a whole subtree
/// from BOTH snapshots and `fs::read(..).unwrap_or_default()` would record an
/// unreadable file as empty on both sides — each turns a real mutation into a
/// false "unchanged". An entry kind with no deterministic exact representation
/// panics rather than being silently omitted.
fn tree_snapshot(root: &Path) -> Vec<TreeEntry> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<TreeEntry>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| {
            panic!(
                "nonmutation oracle: read_dir({}) failed: {e} — the oracle must fail \
                 loudly, never silently skip a subtree",
                dir.display()
            )
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| {
                panic!(
                    "nonmutation oracle: a directory entry of {} could not be read: {e}",
                    dir.display()
                )
            });
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap_or_else(|_| {
                    panic!(
                        "nonmutation oracle: {} is not under the snapshot base {}",
                        path.display(),
                        base.display()
                    )
                })
                .to_path_buf();
            // `symlink_metadata` describes the LINK itself, so a symlink is never
            // resolved to its destination here.
            let meta = fs::symlink_metadata(&path).unwrap_or_else(|e| {
                panic!(
                    "nonmutation oracle: symlink_metadata({}) failed: {e}",
                    path.display()
                )
            });
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                let target = fs::read_link(&path).unwrap_or_else(|e| {
                    panic!(
                        "nonmutation oracle: read_link({}) failed: {e}",
                        path.display()
                    )
                });
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::Symlink(target),
                });
            } else if file_type.is_dir() {
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::Directory,
                });
                walk(&path, base, out);
            } else if file_type.is_file() {
                let bytes = fs::read(&path).unwrap_or_else(|e| {
                    panic!(
                        "nonmutation oracle: read({}) failed: {e} — an unreadable file \
                         must not be recorded as empty bytes",
                        path.display()
                    )
                });
                out.push(TreeEntry {
                    rel,
                    kind: EntryKind::RegularFile(bytes),
                });
            } else {
                panic!(
                    "nonmutation oracle: unsupported entry kind at {} ({file_type:?}); no \
                     deterministic exact representation exists, so the oracle refuses to \
                     omit it",
                    path.display()
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Assert two [`tree_snapshot`]s are EXACTLY equal, naming only the changed
/// entries: a bare `assert_eq!` would dump every file's contents (the golden DB
/// included) into the failure message.
fn assert_tree_unchanged(before: &[TreeEntry], after: &[TreeEntry], context: &str) {
    let mut diffs: Vec<String> = Vec::new();
    let before_map: std::collections::BTreeMap<&Path, &EntryKind> =
        before.iter().map(|e| (e.rel.as_path(), &e.kind)).collect();
    let after_map: std::collections::BTreeMap<&Path, &EntryKind> =
        after.iter().map(|e| (e.rel.as_path(), &e.kind)).collect();
    for (path, kind) in &before_map {
        match after_map.get(path) {
            None => diffs.push(format!(
                "removed: {} ({})",
                path.display(),
                kind_label(kind)
            )),
            Some(other) if other != kind => diffs.push(format!(
                "changed: {} ({} -> {})",
                path.display(),
                kind_label(kind),
                kind_label(other)
            )),
            Some(_) => {}
        }
    }
    for (path, kind) in &after_map {
        if !before_map.contains_key(path) {
            diffs.push(format!(
                "created: {} ({})",
                path.display(),
                kind_label(kind)
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "{context}: the project tree must be EXACTLY unchanged (complete file bytes, \
         directory presence, and symlink targets compared — never sizes), but \
         changed: {diffs:?}"
    );
}

/// An INVALID `CODEGRAPH_DIR` must fail closed on the REAL MCP public surface —
/// it must not silently fall back to the default `.codegraph` namespace.
///
/// A TRAP database is staged at the default location `<project>/.codegraph/
/// codegraph.db` (the very path a `.`-alias fallback would open) and
/// `CODEGRAPH_DIR=.` is set, which `IndexPaths::resolve` refuses because it
/// aliases the project root. A real `tools/call codegraph_search` must then:
/// surface the actionable invalid-configuration diagnostic, NOT the generic "No
/// indexed project" miss; serve NONE of the trap DB's symbols; and mutate zero
/// bytes of the project tree (trap DB included).
#[test]
fn invalid_configured_root_mcp_fails_closed_and_never_serves_trap_default_db() {
    let dir = unique_dir("trap");
    let project = dir.path.join("mini");
    fs::create_dir_all(&project).unwrap();

    let trap_paths = codegraph_core::IndexPaths::resolve(&project, None).unwrap();
    let trap_db = trap_paths.current_db();
    stage_mini_db_at(&trap_paths, &project);
    assert!(
        trap_db.is_file(),
        "sanity: the trap DB must exist at the default namespace a fallback would open"
    );

    let _env = EnvGuard::set(".");
    assert!(
        codegraph_core::IndexPaths::resolve(&project, Some(".")).is_err(),
        "sanity: `CODEGRAPH_DIR=.` must be refused by the path authority"
    );

    let before = tree_snapshot(&project);
    let resp = tool_call_search(&project, project.to_str().unwrap());
    let after = tree_snapshot(&project);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let whole = serde_json::to_string(&resp).unwrap();

    assert_eq!(
        resp["result"]["isError"],
        json!(true),
        "an invalid configured root must make the tool call fail closed: {whole}"
    );
    assert!(
        text.contains("CODEGRAPH_DIR"),
        "the error must name the offending configuration: {text}"
    );
    assert!(
        text.contains("must be one non-empty project-local directory name"),
        "the error must carry the stable `IndexPaths` reason, not a generic message: {text}"
    );
    assert!(
        !text.contains("No indexed project"),
        "an invalid configuration must NOT masquerade as an un-init'd project: {text}"
    );
    assert!(
        !whole.contains("Counter") && !whole.contains("increment") && !whole.contains("math.ts"),
        "the trap DB at the default namespace must NEVER be served: {whole}"
    );

    assert_tree_unchanged(
        &before,
        &after,
        "a fail-closed tool call must leave the project tree (trap DB included) unchanged",
    );
}

/// The rmcp front-end (the SHIPPED transport) must fail closed identically: the
/// same trap DB at the default namespace, the same refused `CODEGRAPH_DIR=.`, a
/// real rmcp `tools/call` over a duplex transport. Proves the invalid-config
/// state survives BOTH tool-call paths, not just the hand-rolled one.
#[test]
fn invalid_configured_root_rmcp_fails_closed_and_never_serves_trap_default_db() {
    let dir = unique_dir("trap-rmcp");
    let project = dir.path.join("mini");
    fs::create_dir_all(&project).unwrap();
    let trap_paths = codegraph_core::IndexPaths::resolve(&project, None).unwrap();
    stage_mini_db_at(&trap_paths, &project);

    let _env = EnvGuard::set(".");

    let before = tree_snapshot(&project);
    let resp = rmcp_tool_call_search(&project);
    let after = tree_snapshot(&project);

    let whole = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        resp["isError"],
        json!(true),
        "rmcp must fail closed on an invalid configured root: {whole}"
    );
    let text = resp["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("CODEGRAPH_DIR")
            && text.contains("must be one non-empty project-local directory name"),
        "rmcp must surface the actionable IndexPaths diagnostic: {text}"
    );
    assert!(
        !text.contains("No indexed project"),
        "rmcp must not mask an invalid configuration as an un-init'd project: {text}"
    );
    assert!(
        !whole.contains("Counter") && !whole.contains("increment") && !whole.contains("math.ts"),
        "rmcp must NEVER serve the trap DB at the default namespace: {whole}"
    );
    assert_tree_unchanged(
        &before,
        &after,
        "a fail-closed rmcp tool call must mutate zero project bytes",
    );
}

/// Drive ONE real rmcp `tools/call codegraph_search` against `project` over a
/// `tokio::io::duplex` transport, returning the serialized `CallToolResult`.
fn rmcp_tool_call_search(project: &Path) -> Value {
    use rmcp::ServiceExt;
    use rmcp::model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, ProtocolVersion,
    };

    let project = project.to_path_buf();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let handler =
            codegraph_mcp::rmcp_handler::CodeGraphHandler::new(Some(project.to_path_buf()));
        let server = tokio::spawn(async move {
            if let Ok(running) = handler.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("cfgroot", "0"),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .serve(client_io)
        .await
        .expect("rmcp handshake");

        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), json!("add"));
        args.insert(
            "projectPath".to_string(),
            json!(project.to_str().expect("utf8 project path")),
        );
        let result = client
            .call_tool(
                CallToolRequestParams::new("codegraph_search".to_string()).with_arguments(args),
            )
            .await
            .expect("rmcp call_tool");
        let value = serde_json::to_value(&result).expect("serialize CallToolResult");
        let _ = client.cancel().await;
        let _ = server.await;
        value
    })
}
