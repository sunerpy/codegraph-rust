//! `sync` stays equal to `index --force` when the project is indexed through
//! symlinks (upstream #935).
//!
//! The scan follows symlinked files and directories, so retargeting a link can
//! point an indexed logical path at a different file with the same size and
//! mtime. The stat pre-filter must not keep the old graph then: every full
//! build records the links it followed, and a full sync re-reads whatever a
//! new, retargeted or unrecorded link reaches.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::Store;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-symlink-sync-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    assert!(
        output.status.success(),
        "codegraph {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn touch(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

/// Create a symlink. Without the privilege (Windows) a local run skips the
/// test, but CI must exercise it, so there a refusal fails loudly.
fn link(target: &Path, at: &Path, dir: bool) -> bool {
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    #[cfg(unix)]
    let made = {
        let _ = dir;
        std::os::unix::fs::symlink(target, at)
    };
    #[cfg(windows)]
    let made = if dir {
        std::os::windows::fs::symlink_dir(target, at)
    } else {
        std::os::windows::fs::symlink_file(target, at)
    };
    match made {
        Ok(()) => true,
        Err(error) if std::env::var_os("CI").is_some() => {
            panic!("CI must be able to create symlinks: {error}")
        }
        Err(_) => false,
    }
}

fn remove_link(at: &Path) {
    // A directory symlink is a directory entry on Windows, a file on Unix.
    if fs::remove_file(at).is_err() {
        fs::remove_dir(at).expect("remove link");
    }
}

/// Give `path` exactly `like`'s modification time.
fn copy_mtime(like: &Path, path: &Path) {
    let modified = fs::metadata(like).unwrap().modified().unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn store(project: &Path) -> Store {
    let paths = IndexPaths::resolve(project, None).expect("resolve index paths");
    Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("open the index for reading")
}

/// Files (path, hash) and nodes (file, kind, name, line), sorted.
fn snapshot(project: &Path) -> (Vec<(String, String)>, Vec<(String, String, String, i64)>) {
    let store = store(project);
    let mut files: Vec<(String, String)> = store
        .all_files()
        .unwrap()
        .into_iter()
        .map(|file| (file.path, file.content_hash))
        .collect();
    files.sort();
    let mut nodes: Vec<(String, String, String, i64)> = store
        .all_nodes()
        .unwrap()
        .into_iter()
        .map(|node| {
            (
                node.file_path,
                format!("{:?}", node.kind),
                node.name,
                node.start_line,
            )
        })
        .collect();
    nodes.sort();
    (files, nodes)
}

fn has_symbol(project: &Path, name: &str) -> bool {
    snapshot(project).1.iter().any(|node| node.2 == name)
}

fn modified_paths(project: &str) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(&cli(&["status", project, "--json"])).expect("status JSON");
    value["pendingChanges"]["modifiedPaths"]
        .as_array()
        .expect("modifiedPaths")
        .iter()
        .map(|path| path.as_str().unwrap().to_string())
        .collect()
}

/// `sync` then `index --force` must leave identical graphs.
fn assert_sync_equals_index_force(project: &Path) {
    let p = project.to_str().unwrap();
    cli(&["sync", p]);
    let synced = snapshot(project);
    cli(&["index", "--force", p]);
    assert_eq!(synced, snapshot(project), "sync must equal index --force");
}

#[test]
fn init_indexes_through_links_and_a_removed_link_leaves_the_index() {
    let dir = TestDir::new("basic");
    let project = dir.path.join("proj");
    let outside = dir.path.join("outside");
    touch(
        &project,
        "src/app.ts",
        "export function app() { return 1; }\n",
    );
    touch(
        &outside,
        "lib/util.ts",
        "export function util() { return 2; }\n",
    );
    touch(
        &project,
        "src/real.ts",
        "export function real() { return 3; }\n",
    );
    if !link(&outside.join("lib"), &project.join("vendored"), true)
        || !link(
            &project.join("src/real.ts"),
            &project.join("alias.ts"),
            false,
        )
    {
        return;
    }
    let p = project.to_str().unwrap();
    cli(&["init", p]);
    let files: Vec<String> = snapshot(&project).0.into_iter().map(|f| f.0).collect();
    assert_eq!(
        files,
        vec!["alias.ts", "src/app.ts", "src/real.ts", "vendored/util.ts"]
    );
    let recorded = store(&project)
        .get_project_metadata(codegraph_watch::FOLLOWED_LINKS_KEY)
        .unwrap()
        .expect("a full build records the links it followed");
    assert!(
        recorded.contains("vendored") && recorded.contains("alias.ts"),
        "{recorded}"
    );

    remove_link(&project.join("vendored"));
    assert_sync_equals_index_force(&project);
    assert!(
        !has_symbol(&project, "util"),
        "a removed link's files leave the index"
    );
}

#[test]
fn a_retargeted_directory_link_is_reread_despite_an_equal_stat() {
    let dir = TestDir::new("retarget-dir");
    let project = dir.path.join("proj");
    let outside = dir.path.join("outside");
    touch(&project, "src/app.ts", "export const app = 1;\n");
    let old = touch(&outside, "v1/x.ts", "export const aa = 1;\n");
    let new = touch(&outside, "v2/x.ts", "export const bb = 1;\n");
    copy_mtime(&old, &new);
    if !link(&outside.join("v1"), &project.join("lib"), true) {
        return;
    }
    let p = project.to_str().unwrap();
    cli(&["init", p]);
    assert!(has_symbol(&project, "aa"));

    remove_link(&project.join("lib"));
    assert!(link(&outside.join("v2"), &project.join("lib"), true));
    assert_eq!(modified_paths(p), vec!["lib/x.ts".to_string()]);
    cli(&["sync", p]);
    assert!(has_symbol(&project, "bb") && !has_symbol(&project, "aa"));
    assert_sync_equals_index_force(&project);
}

#[test]
fn a_retargeted_file_link_is_reread_despite_an_equal_stat() {
    let dir = TestDir::new("retarget-file");
    let project = dir.path.join("proj");
    let outside = dir.path.join("outside");
    touch(&project, "src/app.ts", "export const app = 1;\n");
    let old = touch(&outside, "f1.ts", "export const ff = 1;\n");
    let new = touch(&outside, "f2.ts", "export const gg = 1;\n");
    copy_mtime(&old, &new);
    if !link(&old, &project.join("f.ts"), false) {
        return;
    }
    let p = project.to_str().unwrap();
    cli(&["init", p]);
    assert!(has_symbol(&project, "ff"));

    remove_link(&project.join("f.ts"));
    assert!(link(&new, &project.join("f.ts"), false));
    assert_eq!(modified_paths(p), vec!["f.ts".to_string()]);
    cli(&["sync", p]);
    assert!(has_symbol(&project, "gg") && !has_symbol(&project, "ff"));
    assert_sync_equals_index_force(&project);
}

/// An index without the record — built before it existed — may track a
/// regular path that was later replaced by a link to a same-stat file.
#[test]
fn an_index_without_the_link_record_rereads_every_link() {
    let dir = TestDir::new("no-record");
    let project = dir.path.join("proj");
    let other = dir.path.join("other");
    let x = touch(&project, "src/x.ts", "export const xx = 1;\n");
    let y = touch(&project, "lib/y.ts", "export const yy = 1;\n");
    let x2 = touch(&other, "x.ts", "export const x2 = 1;\n");
    let y2 = touch(&other, "lib/y.ts", "export const y2 = 1;\n");
    copy_mtime(&x, &x2);
    copy_mtime(&y, &y2);
    let p = project.to_str().unwrap();
    cli(&["init", p]);

    // Forget the record, as an index from before this change never wrote it.
    let paths = IndexPaths::resolve(&project, None).unwrap();
    {
        let db = rusqlite::Connection::open(paths.current_db()).unwrap();
        db.execute(
            "DELETE FROM project_metadata WHERE key = ?1",
            [codegraph_watch::FOLLOWED_LINKS_KEY],
        )
        .unwrap();
    }
    fs::remove_file(&x).unwrap();
    fs::remove_dir_all(project.join("lib")).unwrap();
    if !link(&x2, &project.join("src/x.ts"), false)
        || !link(&other.join("lib"), &project.join("lib"), true)
    {
        return;
    }

    assert_eq!(
        modified_paths(p),
        vec!["lib/y.ts".to_string(), "src/x.ts".to_string()]
    );
    cli(&["sync", p]);
    assert!(has_symbol(&project, "x2") && has_symbol(&project, "y2"));
    assert!(!has_symbol(&project, "xx") && !has_symbol(&project, "yy"));
    assert!(
        store(&project)
            .get_project_metadata(codegraph_watch::FOLLOWED_LINKS_KEY)
            .unwrap()
            .is_some(),
        "the first full sync records the links"
    );
    assert_sync_equals_index_force(&project);
}
