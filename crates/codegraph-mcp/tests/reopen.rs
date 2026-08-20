//! T6 (#925): a long-lived `McpServer` serves replacement database contents
//! without retaining a `CodeGraphEngine`, SQLite connection, or read lease
//! between requests.
//!
//! The decision is keyed on the db file IDENTITY (unix inode / a content-based
//! signature on non-unix), NOT on modified-time: an in-place WAL write bumps
//! mtime but does not replace the file, so it must NOT record a change. On
//! non-unix the signature is a hash of the WAL-stable SQLite header slices
//! (page count + schema cookie + structural header), which is deterministic and
//! mtime-independent — it changes only when the db is rebuilt.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use codegraph_core::node_id::hash_content;
use codegraph_core::types::FileRecord;
use codegraph_extract::engine::{detect_language, extract_file};
use codegraph_mcp::McpServer;
use codegraph_mcp::server::db_identity_change_count;

use codegraph_store::Store;
use serde_json::{Value, json};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn counter_test_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
        "cg-mcp-identity-{tag}-{}-{nanos}-{seq}",
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
        let metadata = fs::metadata(base.join(rel)).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let result = extract_file(base, rel).unwrap();
        store.upsert_nodes(&result.nodes).unwrap();
        all_edges.extend(result.edges);
        store
            .upsert_file(&FileRecord {
                path: (*rel).to_string(),
                content_hash: hash_content(src),
                language: detect_language(rel),
                size: metadata.len() as i64,
                modified_at,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
                generated: false,
            })
            .unwrap();
    }
    store.insert_edges(&all_edges).unwrap();
    drop(store);
    let paths = codegraph_core::IndexPaths::resolve(base, None).unwrap();
    codegraph_store::test_support::finalize_current_test_fixture(&paths).unwrap();
}

/// Drive one `codegraph_search` `tools/call` against `server`, returning the
/// rendered text body. Reusing the SAME server proves request-scoped engines do
/// not leave stale graph state attached to the long-lived protocol session.
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
        .run_until_adoption(Cursor::new(input.into_bytes()), &mut output)
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
fn same_server_reads_replacement_graph_without_close_helper() {
    let _counter = counter_test_guard();
    // Given: an indexed project whose db (inode A) contains `alphaSymbol`.
    let project = TestProject {
        path: unique_base("replace"),
    };
    index_into(
        project.path(),
        &[("src/a.ts", "export function alphaSymbol() {}\n")],
    );

    let mut server = McpServer::new(Some(project.path().to_path_buf()));

    let first = search(&mut server, project.path(), "alphaSymbol");
    assert!(
        first.contains("alphaSymbol"),
        "first call should find the original symbol; got:\n{first}"
    );
    let changes_before = db_identity_change_count();
    let identity_before = db_identity(project.path());

    // When: the db is REPLACED on disk with a fresh index whose content differs
    // (a new symbol `betaSymbol`). No compatibility helper participates: every
    // request must already have dropped its engine, SQLite handles, and lease.
    fs::remove_dir_all(project.path().join(".codegraph")).unwrap();
    index_into(
        project.path(),
        &[("src/b.ts", "export function betaSymbol() {}\n")],
    );
    let identity_after = db_identity(project.path());

    // Then: the SAME server's next request serves only the replacement graph.
    let after = search(&mut server, project.path(), "betaSymbol");
    let changes_after = db_identity_change_count();
    #[cfg(unix)]
    assert_eq!(
        changes_after - changes_before,
        u64::from(identity_before != identity_after),
        "the observation counter must match the inode actually allocated \
         (before={changes_before}, after={changes_after})"
    );
    #[cfg(not(unix))]
    let _ = (
        changes_before,
        changes_after,
        identity_before,
        identity_after,
    );

    assert!(
        after.contains("betaSymbol"),
        "after replacement the next request must serve the new index; got:\n{after}"
    );
    assert!(
        !after.contains("alphaSymbol"),
        "after replacement the old graph must not leak into the response; got:\n{after}"
    );
    assert!(
        !after.to_lowercase().contains("failed to open"),
        "the replacement request must not surface an open error; got:\n{after}"
    );
}

#[test]
fn unchanged_identity_does_not_increment_observation_count() {
    let _counter = counter_test_guard();
    // Given: one request establishes the server's identity observation baseline.
    let project = TestProject {
        path: unique_base("stable"),
    };
    index_into(
        project.path(),
        &[("src/a.ts", "export function gammaSymbol() {}\n")],
    );

    let mut server = McpServer::new(Some(project.path().to_path_buf()));

    let _ = search(&mut server, project.path(), "gammaSymbol");
    let after_first = db_identity_change_count();

    // When: more calls run WITHOUT replacing the db (same file), even after an
    // in-place mtime bump (a normal WAL write) — that must NOT be treated as a
    // replace. First prove the identity itself is content-neutral under a pure
    // mtime touch (the header bytes are unchanged, so the signature holds).
    let db = project.path().join(".codegraph").join("codegraph.db");
    let id_before_touch = db_identity(project.path());
    filetouch(&db);
    assert_eq!(
        id_before_touch,
        db_identity(project.path()),
        "an mtime-only touch (rewriting identical bytes) must not change DbIdentity"
    );
    for _ in 0..5 {
        let _ = search(&mut server, project.path(), "gammaSymbol");
    }
    let after_many = db_identity_change_count();

    // Then: the stable identity produced no replacement observation.
    assert_eq!(
        after_first, after_many,
        "a same-file (in-place) project must NOT record an identity change \
         (after_first_call={after_first}, after_many={after_many})"
    );
}

#[test]
fn close_cached_handles_resets_diagnostics_without_forcing_replacement() {
    let _counter = counter_test_guard();
    let project = TestProject {
        path: unique_base("close-diagnostic"),
    };
    index_into(
        project.path(),
        &[("src/a.ts", "export function deltaSymbol() {}\n")],
    );
    let mut server = McpServer::new(Some(project.path().to_path_buf()));
    assert!(search(&mut server, project.path(), "deltaSymbol").contains("deltaSymbol"));
    let before = db_identity_change_count();

    server.close_cached_handles();
    let after = search(&mut server, project.path(), "deltaSymbol");

    assert!(after.contains("deltaSymbol"));
    assert_eq!(
        db_identity_change_count(),
        before,
        "clearing diagnostic identity state must not synthesize a replacement"
    );
}

/// Mirror of the production `DbIdentity` for the project's db file, folded into
/// a single `u128` so the replacement test can condition its metric assertion
/// on the identity the filesystem actually allocated.
/// Unix uses the inode; non-unix mirrors the production `(len, creation_time,
/// header_sig)` fold where `header_sig` hashes the SAME WAL-stable SQLite header
/// slices (`[16..24]`, `[28..32]`, `[40..44]`) — NO mtime, so a pure mtime touch
/// leaves it unchanged while a rebuild changes it.
fn db_identity(project: &Path) -> u128 {
    let db = project.join(".codegraph").join("codegraph.db");
    let meta = fs::metadata(&db).expect("db metadata");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        u128::from(meta.ino())
    }
    #[cfg(all(not(unix), windows))]
    {
        use std::os::windows::fs::MetadataExt;
        (u128::from(meta.len()) << 64)
            ^ (u128::from(meta.creation_time()) << 1)
            ^ u128::from(header_sig(&db))
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        (u128::from(meta.len()) << 64) ^ u128::from(header_sig(&db))
    }
}

/// Hash of the WAL-stable SQLite header slices, mirroring the production
/// `header_sig` in `server.rs`: a short-read-tolerant read of up to 100 header
/// bytes, hashing only `[16..24]`, `[28..32]`, `[40..44]` (page count + schema
/// cookie + structural header — stable on a WAL write, changes on a rebuild).
/// `0` on any failure or a too-short file. Only compiled on non-unix (the unix
/// arm keys on the inode and never reads the header).
#[cfg(not(unix))]
fn header_sig(db: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::io::Read;

    let Ok(mut file) = fs::File::open(db) else {
        return 0;
    };
    let mut header = [0u8; 100];
    let mut filled = 0usize;
    while filled < header.len() {
        match file.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return 0,
        }
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (start, end) in [(16usize, 24usize), (28, 32), (40, 44)] {
        if filled >= end {
            header[start..end].hash(&mut hasher);
        }
    }
    hasher.finish()
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
