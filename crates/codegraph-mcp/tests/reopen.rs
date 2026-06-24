//! T6 (#925): `McpServer` reopens its cached `CodeGraphEngine` when the
//! project's `.codegraph/codegraph.db` is REPLACED on disk (new inode), so a
//! long-lived `serve` never keeps serving a deleted inode.
//!
//! The decision is keyed on the db file IDENTITY (unix inode / windows file
//! index), NOT on modified-time: an in-place WAL write bumps mtime but keeps the
//! same inode, so it must NOT trigger a reopen.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::FileRecord;
use codegraph_extract::engine::{detect_language, extract_file};
use codegraph_mcp::server::reopen_count;
use codegraph_mcp::McpServer;

use codegraph_store::Store;
use serde_json::{json, Value};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Owns a temp project dir and removes it on drop.
struct TestProject {
    path: PathBuf,
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestProject {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn unique_base(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cg-mcp-reopen-{tag}-{}-{nanos}-{seq}",
        std::process::id()
    ))
}

/// Index `files` into `<base>/.codegraph/codegraph.db`, creating a FRESH db file
/// (a new inode each time the `.codegraph` dir was removed first). Mirrors the
/// CLI index order used by the golden harness: nodes upsert, then files, then
/// edges.
fn index_into(base: &Path, files: &[(&str, &str)]) {
    for (rel, src) in files {
        let dst = base.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, src).unwrap();
    }
    let mut store = Store::open(&base.join(".codegraph").join("codegraph.db")).unwrap();
    let mut all_edges = Vec::new();
    for (rel, src) in files {
        let result = extract_file(base, rel).unwrap();
        store.upsert_nodes(&result.nodes).unwrap();
        all_edges.extend(result.edges);
        store
            .upsert_file(&FileRecord {
                path: (*rel).to_string(),
                content_hash: String::new(),
                language: detect_language(rel),
                size: src.len() as i64,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .unwrap();
    }
    store.insert_edges(&all_edges).unwrap();
    drop(store);
}

/// Drive one `codegraph_search` `tools/call` against `server`, returning the
/// rendered text body (NOT a fresh server — reuse the SAME server so its engine
/// cache persists across calls, which is the whole point of the test).
fn search(server: &mut McpServer, project: &Path, query: &str) -> String {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "codegraph_search",
            "arguments": { "query": query, "projectPath": project.to_str().unwrap() }
        }
    });
    let input = format!("{}\n", serde_json::to_string(&req).unwrap());
    let mut output = Vec::new();
    server
        .run(Cursor::new(input.into_bytes()), &mut output)
        .expect("server run");
    let text = String::from_utf8(output).expect("utf8 output");
    let line = text.lines().next().expect("one response line");
    let resp: Value = serde_json::from_str(line).expect("response json");
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn reopens_cached_engine_when_db_replaced_with_new_inode() {
    // Given: an indexed project whose db (inode A) contains `alphaSymbol`.
    let project = TestProject {
        path: unique_base("replace"),
    };
    index_into(
        project.path(),
        &[("src/a.ts", "export function alphaSymbol() {}\n")],
    );

    let mut server = McpServer::new(Some(project.path().to_path_buf()));

    // Populate the engine cache against inode A.
    let first = search(&mut server, project.path(), "alphaSymbol");
    assert!(
        first.contains("alphaSymbol"),
        "first call should find the original symbol; got:\n{first}"
    );
    let reopens_before = reopen_count();

    // When: the db is REPLACED on disk with a fresh index (new inode B) whose
    // content differs (a new symbol `betaSymbol`). Removing the dir first
    // guarantees a new inode for the rebuilt db file.
    let id_a = db_inode(project.path());
    fs::remove_dir_all(project.path().join(".codegraph")).unwrap();
    index_into(
        project.path(),
        &[("src/b.ts", "export function betaSymbol() {}\n")],
    );
    let id_b = db_inode(project.path());
    assert_ne!(
        id_a, id_b,
        "the rebuilt db must have a new inode (replacement)"
    );

    // Then: the SAME server's SAME tool call must REOPEN the engine — without the
    // #925 fix the cached engine keeps serving inode A and `reopen_count` never
    // advances (this delta is the deterministic RED probe).
    let after = search(&mut server, project.path(), "betaSymbol");
    let reopens_after = reopen_count();
    assert_eq!(
        reopens_after - reopens_before,
        1,
        "a db replacement (new inode) must trigger exactly one reopen \
         (before={reopens_before}, after={reopens_after})"
    );

    // And: it returns FRESH data from inode B, not an open error.
    assert!(
        after.contains("betaSymbol"),
        "after replacement the engine must reopen and serve the new index; got:\n{after}"
    );
    assert!(
        !after.to_lowercase().contains("failed to open"),
        "reopen must not surface an open error; got:\n{after}"
    );
}

#[test]
fn does_not_reopen_when_inode_is_unchanged() {
    // Given: an indexed project; one tool call populates the cache (one open,
    // which is NOT counted as a reopen).
    let project = TestProject {
        path: unique_base("stable"),
    };
    index_into(
        project.path(),
        &[("src/a.ts", "export function gammaSymbol() {}\n")],
    );

    let mut server = McpServer::new(Some(project.path().to_path_buf()));

    let _ = search(&mut server, project.path(), "gammaSymbol");
    let after_first = reopen_count();

    // When: more calls run WITHOUT replacing the db (same inode), even after an
    // in-place mtime bump (a normal WAL write) — that must NOT be treated as a
    // replace.
    filetouch(&project.path().join(".codegraph").join("codegraph.db"));
    for _ in 0..5 {
        let _ = search(&mut server, project.path(), "gammaSymbol");
    }
    let after_many = reopen_count();

    // Then: the engine identity is stable — no reopen fired after the initial
    // open, despite the changed mtime.
    assert_eq!(
        after_first, after_many,
        "a same-inode (in-place) project must NOT trigger any reopen \
         (after_first_call={after_first}, after_many={after_many})"
    );
}

/// Inode (unix) / file-len proxy (other) of the project's db file, used only to
/// confirm the test's own replacement actually changed the inode.
fn db_inode(project: &Path) -> u64 {
    let db = project.join(".codegraph").join("codegraph.db");
    let meta = fs::metadata(&db).expect("db metadata");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Bump the db file's modified-time without changing its inode, simulating a
/// normal in-place WAL write: rewrite the same bytes via `O_WRONLY` (no new
/// file, no truncation of a fresh inode) so the inode is preserved.
fn filetouch(path: &Path) {
    use std::io::Write;
    let bytes = fs::read(path).expect("read db");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open db rw");
    f.write_all(&bytes).expect("rewrite db");
    f.flush().expect("flush db");
}
