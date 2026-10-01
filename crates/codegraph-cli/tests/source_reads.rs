//! Source files are read as upstream reads them (v1.6.1 #1910): never past the
//! size limit, with a size stamp standing in for an oversize file's content;
//! an MPEG transport stream named `.ts` is video, not TypeScript; and bytes
//! that are not UTF-8 are decoded with replacement characters, so no single
//! file can fail an index.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_core::node_id::hash_content;
use codegraph_store::Store;

struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-source-reads-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run codegraph");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// 64 transport-stream packets: the 0x47 sync byte, then a deterministic
/// pseudo-random payload standing in for compressed video.
fn mpeg_ts_clip() -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    let mut payload = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state.to_le_bytes()[0]
    };
    (0..64)
        .flat_map(|_| {
            std::iter::once(0x47)
                .chain((0..187).map(|_| payload()))
                .collect::<Vec<_>>()
        })
        .collect()
}

const ONE_MIB: usize = 1024 * 1024;

fn file_paths(store: &Store) -> Vec<String> {
    store
        .all_files()
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .collect()
}

#[test]
fn index_and_sync_treat_video_latin1_and_oversize_files_like_upstream() {
    let dir = TestDir::new("index");
    dir.write("src/app.ts", "export function app() { return 1; }\n");
    dir.write("testdata/clip.ts", mpeg_ts_clip());
    dir.write(
        "legacy/latin1.c",
        b"/* caf\xe9 */\nint legacy_entry(void) { return 0; }\n",
    );
    let oversize = format!("export const big = 1;\n{}", " ".repeat(ONE_MIB));
    dir.write("big/bundle.ts", &oversize);
    let path = dir.0.to_str().unwrap();

    let (stdout, stderr, ok) = run(&dir.0, &["init", path]);
    assert!(ok, "init failed: {stdout} {stderr}");
    // The oversize file is recorded as skipped; the clip is not counted at all.
    assert!(stdout.contains("Indexed 2 files"), "stdout={stdout}");
    assert!(stdout.contains("Skipped 1 files"), "stdout={stdout}");

    let db = dir.0.join(".codegraph").join("codegraph.db");
    {
        let store = Store::open(&db).unwrap();
        assert_eq!(
            file_paths(&store),
            vec!["big/bundle.ts", "legacy/latin1.c", "src/app.ts"]
        );
        let bundle = store.file_by_path("big/bundle.ts").unwrap().unwrap();
        assert_eq!(
            bundle.content_hash,
            hash_content(&format!("codegraph:oversize:{}", oversize.len()))
        );
        assert!(
            store
                .nodes_by_name("legacy_entry")
                .unwrap()
                .iter()
                .any(|node| node.file_path == "legacy/latin1.c")
        );
    }

    let (stdout, stderr, ok) = run(&dir.0, &["status", path, "--json"]);
    assert!(ok, "status failed: {stdout} {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        status["pendingChanges"]["addedPaths"],
        serde_json::json!([]),
        "an untracked clip is not pending source: {stdout}"
    );

    // A same-size rewrite of an oversize file is not a change; a tracked `.ts`
    // that became a clip is removed like a deletion.
    let rewritten = format!("export const BIG = 2;\n{}", " ".repeat(ONE_MIB));
    assert_eq!(rewritten.len(), oversize.len());
    dir.write("big/bundle.ts", &rewritten);
    dir.write("src/app.ts", mpeg_ts_clip());
    let (stdout, stderr, ok) = run(&dir.0, &["sync", path]);
    assert!(ok, "sync failed: {stdout} {stderr}");
    {
        let store = Store::open(&db).unwrap();
        assert_eq!(file_paths(&store), vec!["big/bundle.ts", "legacy/latin1.c"]);
    }
    let (stdout, stderr, ok) = run(&dir.0, &["status", path, "--json"]);
    assert!(ok, "status failed: {stdout} {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    for kind in ["addedPaths", "modifiedPaths", "removedPaths"] {
        assert_eq!(
            status["pendingChanges"][kind],
            serde_json::json!([]),
            "{kind}: {stdout}"
        );
    }

    // Crossing the limit is a change in either direction.
    dir.write("big/bundle.ts", "export const small = 3;\n");
    let (stdout, stderr, ok) = run(&dir.0, &["sync", path]);
    assert!(ok, "sync failed: {stdout} {stderr}");
    let store = Store::open(&db).unwrap();
    assert!(
        store
            .nodes_by_name("small")
            .unwrap()
            .iter()
            .any(|node| node.file_path == "big/bundle.ts")
    );
}
