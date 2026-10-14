//! The git fast path for pending status (upstream #1829 / #1878).
//!
//! Every case compares the reported changes with the full inventory, the
//! reference answer. Cases git can see must take the fast path; cases it might
//! not see must decline to the full inventory.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use codegraph_core::IndexPaths;
use codegraph_store::Store;
use codegraph_watch::git_pending_hooks;
use codegraph_watch::{
    GIT_PENDING_KEY, PendingChanges, PendingSource, pending_full_inventory,
    pending_project_changes_detailed, sync_changed_paths, sync_project_once,
};

/// Repository-selection variables a git hook exports; a test repo must ignore
/// the ambient one (the pre-push hook runs this suite).
const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// The writer hooks are process-global: tests that set them, or that depend on
/// them being unset, run one at a time.
static HOOKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An empty `XDG_CONFIG_HOME`: no global git attributes or config leak in.
fn isolated_config_home() -> PathBuf {
    std::env::temp_dir().join(format!("cg_git_pending_xdg_{}", std::process::id()))
}

fn hooks_guard() -> std::sync::MutexGuard<'static, ()> {
    static ISOLATE: std::sync::Once = std::sync::Once::new();
    let guard = HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ISOLATE.call_once(|| {
        let home = isolated_config_home();
        fs::create_dir_all(&home).unwrap();
        // SAFETY: every test here holds `HOOKS`, so none runs alongside.
        unsafe {
            // A system attributes file (Git for Windows ships one) or a global
            // one would make every case decline; the decline tests add their own.
            std::env::set_var("GIT_ATTR_NOSYSTEM", "1");
            std::env::set_var("XDG_CONFIG_HOME", &home);
        }
    });
    guard
}

fn codegraph_dir() -> Option<String> {
    std::env::var("CODEGRAPH_DIR").ok()
}

/// Sets `CODEGRAPH_DIR` for one test (every test here holds [`hooks_guard`],
/// so none runs alongside) and restores it on drop.
struct IndexDirGuard(Option<std::ffi::OsString>);

impl IndexDirGuard {
    fn set(name: &str) -> Self {
        let previous = std::env::var_os("CODEGRAPH_DIR");
        // SAFETY: tests in this binary are serialized by `hooks_guard`.
        unsafe { std::env::set_var("CODEGRAPH_DIR", name) };
        Self(previous)
    }
}

impl Drop for IndexDirGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("CODEGRAPH_DIR", value),
                None => std::env::remove_var("CODEGRAPH_DIR"),
            }
        }
    }
}

/// The record straight from the database file, readable even when an
/// interrupted writer left the namespace unreadable through the state gate.
fn raw_record(root: &Path) -> Option<serde_json::Value> {
    let db = IndexPaths::resolve(root, codegraph_dir().as_deref())
        .unwrap()
        .current_db();
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT value FROM project_metadata WHERE key = ?1",
        [GIT_PENDING_KEY],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(|value| serde_json::from_str(&value).unwrap())
}

/// A path git accepts as a clone source on every platform: no Windows
/// verbatim prefix, forward slashes.
fn git_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("//?/")
        .map_or(text.clone(), str::to_string)
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

struct Repo {
    /// The project root (a subdirectory of the work tree for `in_subdir`).
    root: PathBuf,
    top: PathBuf,
}

impl Repo {
    fn new(tag: &str) -> Self {
        Self::with_project(tag, None)
    }

    fn in_subdir(tag: &str) -> Self {
        Self::with_project(tag, Some("packages/app"))
    }

    fn with_project(tag: &str, project: Option<&str>) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let top = std::env::temp_dir().join(format!(
            "cg_git_pending_{tag}_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&top).unwrap();
        let top = top.canonicalize().unwrap();
        let root = match project {
            Some(sub) => top.join(sub),
            None => top.clone(),
        };
        fs::create_dir_all(&root).unwrap();
        let repo = Self { root, top };
        repo.git_at(&repo.top, &["init", "-q", "."]);
        // Keep the index itself out of every commit, as a project does.
        fs::write(repo.top.join(".git/info/exclude"), ".codegraph/\n").unwrap();
        // The same byte- and case-exact git on every runner (Windows defaults
        // to `core.autocrlf=true`, macOS and Windows to `core.ignorecase`);
        // the decline tests set them back on purpose.
        for (key, value) in [
            ("core.autocrlf", "false"),
            ("core.ignorecase", "false"),
            ("core.precomposeunicode", "false"),
        ] {
            repo.git_at(&repo.top, &["config", key, value]);
        }
        repo
    }

    fn git_at(&self, dir: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        command.current_dir(dir);
        for var in GIT_ENV {
            command.env_remove(var);
        }
        let output = command
            .args([
                "-c",
                "user.name=cg",
                "-c",
                "user.email=cg@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_at(&self.top, args)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }

    /// A full sync; on a fresh project it builds the index.
    fn sync(&self) {
        sync_project_once(&self.root).expect("full sync");
    }

    fn sync_paths(&self, paths: &[&str]) {
        let db = IndexPaths::resolve(&self.root, codegraph_dir().as_deref())
            .unwrap()
            .current_db();
        sync_changed_paths(
            &self.root,
            db,
            paths.iter().map(|path| self.root.join(path)),
        )
        .expect("incremental sync");
    }

    fn store(&self) -> Store {
        let paths = IndexPaths::resolve(&self.root, codegraph_dir().as_deref()).unwrap();
        Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
            .expect("open the index")
    }

    fn pending(&self) -> (PendingChanges, PendingSource) {
        pending_project_changes_detailed(&self.root, &self.store()).expect("pending")
    }

    fn full(&self) -> PendingChanges {
        pending_full_inventory(&self.root, &self.store()).expect("full inventory")
    }

    fn record(&self) -> Option<serde_json::Value> {
        self.store()
            .get_project_metadata(GIT_PENDING_KEY)
            .unwrap()
            .map(|value| serde_json::from_str(&value).unwrap())
    }

    fn dirty(&self) -> BTreeSet<String> {
        self.record().expect("a git record")["dirty"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| path.as_str().unwrap().to_string())
            .collect()
    }

    /// The fast path answers, and agrees with the full inventory.
    fn assert_fast(&self, context: &str) -> PendingChanges {
        let (pending, source) = self.pending();
        assert_eq!(
            source,
            PendingSource::GitFastPath,
            "{context}: fast path taken"
        );
        assert_eq!(
            pending,
            self.full(),
            "{context}: fast path equals the inventory"
        );
        pending
    }

    /// The fast path declines, and the answer is the full inventory's.
    fn assert_declines(&self, context: &str) -> PendingChanges {
        let (pending, source) = self.pending();
        assert_eq!(source, PendingSource::FullInventory, "{context}: declined");
        assert_eq!(pending, self.full(), "{context}");
        pending
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.top);
    }
}

/// A project with two committed sources, indexed.
fn indexed(tag: &str) -> Option<Repo> {
    if !git_available() {
        return None;
    }
    let repo = Repo::new(tag);
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.write("src/b.ts", "export const b = 1;\n");
    repo.commit("init");
    repo.sync();
    Some(repo)
}

fn paths(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn clean_edits_and_commits_take_the_fast_path() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("edits") else { return };
    assert!(repo.assert_fast("clean").is_empty());

    repo.write("src/a.ts", "export const a = 2;\n");
    assert_eq!(repo.assert_fast("an edit").modified, paths(&["src/a.ts"]));

    // #1829: committing the edit must not make it read as indexed.
    repo.commit("edit a");
    assert_eq!(
        repo.assert_fast("an edit, committed").modified,
        paths(&["src/a.ts"])
    );

    repo.write("src/c.ts", "export const c = 1;\n");
    repo.commit("add c");
    assert_eq!(
        repo.assert_fast("a new file, committed").added,
        paths(&["src/c.ts"])
    );

    fs::remove_file(repo.root.join("src/b.ts")).unwrap();
    assert_eq!(
        repo.assert_fast("a tracked delete").removed,
        paths(&["src/b.ts"])
    );
}

#[test]
fn a_commit_reverted_by_a_new_commit_reads_clean() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("revert") else {
        return;
    };
    repo.write("src/a.ts", "export const a = 2;\n");
    repo.commit("edit a");
    repo.git(&["revert", "--no-edit", "HEAD"]);
    assert!(repo.assert_fast("edit then revert").is_empty());
}

#[test]
fn a_file_indexed_dirty_then_restored_is_modified() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("restore") else {
        return;
    };
    repo.write("src/a.ts", "export const a = 99;\n");
    repo.sync(); // indexes the dirty content
    repo.git(&["checkout", "--", "."]);
    assert_eq!(repo.assert_fast("restored").modified, paths(&["src/a.ts"]));
}

#[test]
fn untracked_files_are_followed_through_add_edit_and_delete() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("untracked") else {
        return;
    };
    repo.write("src/u.ts", "export const u = 1;\n");
    assert_eq!(
        repo.assert_fast("untracked add").added,
        paths(&["src/u.ts"])
    );
    repo.sync();
    repo.write("src/u.ts", "export const u = 22;\n");
    assert_eq!(
        repo.assert_fast("untracked edit").modified,
        paths(&["src/u.ts"])
    );
    fs::remove_file(repo.root.join("src/u.ts")).unwrap();
    assert_eq!(
        repo.assert_fast("untracked delete").removed,
        paths(&["src/u.ts"])
    );
}

#[test]
fn an_amended_stamp_that_still_exists_keeps_the_fast_path() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("amend") else { return };
    repo.write("src/a.ts", "export const a = 3;\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "--amend", "--no-edit"]);
    assert_eq!(repo.assert_fast("amended").modified, paths(&["src/a.ts"]));
}

#[test]
fn an_incremental_sync_then_a_commit_reads_clean() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("incremental") else {
        return;
    };
    repo.write("src/a.ts", "export const a = 4;\n");
    repo.sync_paths(&["src/a.ts"]);
    assert!(
        repo.dirty().contains("src/a.ts"),
        "the requested path is recorded"
    );
    repo.commit("edit a");
    assert!(repo.assert_fast("incremental then commit").is_empty());
}

/// The scan keeps `.cache` although the watcher does not watch it.
#[test]
fn a_file_the_watcher_skips_but_the_scan_keeps_is_classified_by_the_scan() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let repo = Repo::new("cache");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.write(".cache/c.ts", "export const c = 1;\n");
    repo.commit("init");
    repo.sync();
    repo.write(".cache/c.ts", "export const c = 2;\n");
    assert_eq!(
        repo.assert_fast(".cache edit").modified,
        paths(&[".cache/c.ts"])
    );
}

#[test]
fn a_project_below_its_work_tree_root_rebases_git_paths() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let repo = Repo::in_subdir("subdir");
    repo.write("src/a.ts", "export const a = 1;\n");
    fs::write(repo.top.join("outside.ts"), "export const o = 1;\n").unwrap();
    repo.commit("init");
    repo.sync();
    assert!(repo.assert_fast("subdir clean").is_empty());
    repo.write("src/a.ts", "export const a = 2;\n");
    repo.commit("edit");
    assert_eq!(
        repo.assert_fast("subdir commit").modified,
        paths(&["src/a.ts"])
    );
    repo.sync_paths(&["src/a.ts"]);
    assert!(
        repo.dirty().contains("src/a.ts"),
        "dirty is project-relative"
    );
    assert!(repo.assert_fast("subdir after incremental").is_empty());

    // An ignored file the scan keeps is invisible to git status: decline.
    fs::write(repo.root.join("src/.gitignore"), "local.ts\n").unwrap();
    repo.write("src/local.ts", "export const l = 1;\n");
    repo.assert_declines("subdir nested .gitignore");
}

#[test]
fn no_git_an_empty_repo_or_no_record_declines() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let plain = std::env::temp_dir().join(format!("cg_git_pending_plain_{}", std::process::id()));
    let _ = fs::remove_dir_all(&plain);
    fs::create_dir_all(plain.join("src")).unwrap();
    fs::write(plain.join("src/a.ts"), "export const a = 1;\n").unwrap();
    sync_project_once(&plain).unwrap();
    let paths = IndexPaths::resolve(&plain, None).unwrap();
    let store =
        Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false).unwrap();
    let (_, source) = pending_project_changes_detailed(&plain, &store).unwrap();
    assert_eq!(source, PendingSource::FullInventory, "not a git work tree");
    drop(store);
    fs::remove_dir_all(&plain).ok();

    let empty = Repo::new("empty");
    empty.write("src/a.ts", "export const a = 1;\n");
    empty.sync();
    empty.assert_declines("a repository without a commit");

    let Some(repo) = indexed("no-record") else {
        return;
    };
    let db = IndexPaths::resolve(&repo.root, codegraph_dir().as_deref())
        .unwrap()
        .current_db();
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "DELETE FROM project_metadata WHERE key = ?1",
            [GIT_PENDING_KEY],
        )
        .unwrap();
    repo.assert_declines("an index from before the record existed");
}

#[test]
fn an_unknown_stamp_or_a_new_scope_declines() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("stamp") else { return };
    // Rebuild on a commit, then make that commit vanish entirely.
    repo.write("src/a.ts", "export const a = 5;\n");
    repo.commit("doomed");
    repo.sync();
    repo.git(&["reset", "-q", "--hard", "HEAD~1"]);
    repo.git(&["reflog", "expire", "--expire=now", "--all"]);
    repo.git(&["gc", "-q", "--prune=now"]);
    repo.assert_declines("the stamp is gone");

    let Some(repo) = indexed("scope") else { return };
    fs::write(repo.root.join(".gitignore"), "src/b.ts\n").unwrap();
    repo.assert_declines("the root .gitignore changed");
}

/// `.git/info/exclude` decides scan membership (upstream #1728) but hides
/// nothing from `git status` that is already tracked, so a record made before
/// an edit to it proves nothing afterwards.
#[test]
fn a_changed_repository_exclude_declines() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("exclude-scope") else {
        return;
    };
    fs::write(
        repo.top.join(".git/info/exclude"),
        ".codegraph/\nsrc/b.ts\n",
    )
    .unwrap();
    let pending = repo.assert_declines("the repository exclude file changed");
    assert_eq!(pending.removed, paths(&["src/b.ts"]));
}

#[test]
fn nested_repositories_submodules_and_hidden_changes_decline() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("nested") else {
        return;
    };
    let nested = repo.root.join("vendored");
    fs::create_dir_all(&nested).unwrap();
    repo.git_at(&nested, &["init", "-q", "."]);
    fs::write(nested.join("v.ts"), "export const v = 1;\n").unwrap();
    repo.assert_declines("an untracked nested repository");

    let Some(repo) = indexed("assume") else {
        return;
    };
    repo.git(&["update-index", "--assume-unchanged", "src/a.ts"]);
    repo.write("src/a.ts", "export const a = 6;\n");
    assert_eq!(
        repo.assert_declines("assume-unchanged").modified,
        paths(&["src/a.ts"])
    );

    if !git_available() {
        return;
    }
    let source = Repo::new("submodule-source");
    source.write("lib.ts", "export const lib = 1;\n");
    source.commit("lib");
    let host = Repo::new("submodule-host");
    host.write("src/a.ts", "export const a = 1;\n");
    host.git(&["submodule", "add", "-q", &git_path(&source.top), "sub"]);
    host.git(&["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"]);
    host.commit("submodule");
    host.sync();
    fs::write(host.root.join("sub/lib.ts"), "export const lib = 2;\n").unwrap();
    assert_eq!(
        host.assert_declines("a submodule with ignore = all")
            .modified,
        paths(&["sub/lib.ts"])
    );
}

#[test]
fn an_ignored_file_the_scan_includes_declines() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let repo = Repo::new("include");
    repo.write(".gitignore", "gen/\n");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.write("gen/keep.ts", "export const k = 1;\n");
    repo.commit("init");
    repo.sync();
    // Configure the include once the index exists, then let a full sync take it.
    repo.write(
        ".codegraph/config.toml",
        "[app]\nname = \"cg\"\n\n[indexing]\ninclude = [\"gen/keep.ts\"]\n",
    );
    repo.sync();
    repo.write("gen/keep.ts", "export const k = 2;\n");
    assert_eq!(
        repo.assert_declines("git ignores a file the scan includes")
            .modified,
        paths(&["gen/keep.ts"])
    );
}

#[test]
fn symlinks_decline() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("links") else { return };
    let outside = repo.top.with_extension("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("o.ts"), "export const o = 1;\n").unwrap();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, repo.root.join("linked"));
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, repo.root.join("linked"));
    match made {
        Ok(()) => {}
        Err(error) if std::env::var_os("CI").is_some() => {
            panic!("CI must be able to create symlinks: {error}")
        }
        Err(_) => return,
    }
    repo.assert_declines("a new symlink to a directory");
    repo.sync();
    assert_eq!(repo.record().unwrap()["links"], serde_json::json!(true));
    repo.assert_declines("an index built through a symlink");
    fs::remove_dir_all(&outside).ok();
}

/// Start a just-written executable once, retrying while the kernel reports it busy, then stop it.
///
/// Another thread of this test binary may fork while the file is still open for writing; the
/// child holds the inherited descriptor until it execs, and running the file meanwhile fails with
/// ETXTBSY. Here that failure looked like a declined git, so the test passed without the timeout
/// ever running. Once one start succeeds no writer is left.
#[cfg(unix)]
fn wait_until_executable(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::process::Command::new(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("cannot run {}: {error}", path.display()),
        }
    }
}

#[cfg(unix)]
#[test]
fn a_hanging_git_declines_within_the_timeout() {
    use std::os::unix::fs::PermissionsExt as _;

    let _hooks = hooks_guard();
    let Some(repo) = indexed("timeout") else {
        return;
    };
    let fake = repo.top.with_extension("fake-git");
    // `exec`, so killing the stand-in kills the sleep too and leaves no orphan behind.
    fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_executable(&fake);
    git_pending_hooks::set_git_program(Some(fake.clone()));
    git_pending_hooks::set_git_timeout(Some(Duration::from_millis(300)));
    let started = Instant::now();
    let declined = repo.pending();
    git_pending_hooks::set_git_program(None);
    git_pending_hooks::set_git_timeout(None);
    assert_eq!(declined.1, PendingSource::FullInventory);
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the stand-in git ran until the timeout, rather than failing to start"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the timeout bounds the wait"
    );
    fs::remove_file(&fake).ok();
}

#[test]
fn a_full_build_and_a_full_sync_write_the_record() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("writers") else {
        return;
    };
    let first = repo.record().expect("a full build writes the record");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(first["commit"], serde_json::json!(head));
    repo.write("src/a.ts", "export const a = 7;\n");
    repo.commit("next");
    repo.sync();
    let second = repo.record().expect("a full sync replaces the record");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(second["commit"], serde_json::json!(head));
}

/// A file read in a transient state and restored before the record is written
/// still reaches `dirty` (its stat moved).
#[test]
fn a_file_changed_and_restored_during_the_build_stays_dirty() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let repo = Repo::new("race-read");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.commit("init");
    let file = repo.root.join("src/a.ts");
    let original = fs::read(&file).unwrap();
    let mutate = file.clone();
    git_pending_hooks::set_after_git_begin(Some(Box::new(move |_| {
        fs::write(&mutate, "export const a = 123;\n").unwrap();
    })));
    let restore = file.clone();
    git_pending_hooks::set_before_git_record(Some(Box::new(move |_| {
        fs::write(&restore, &original).unwrap();
        let later = SystemTime::now() + Duration::from_secs(2);
        fs::File::options()
            .write(true)
            .open(&restore)
            .unwrap()
            .set_modified(later)
            .unwrap();
    })));
    repo.sync();
    git_pending_hooks::set_after_git_begin(None);
    git_pending_hooks::set_before_git_record(None);
    assert!(repo.dirty().contains("src/a.ts"), "{:?}", repo.dirty());
    assert_eq!(
        repo.assert_fast("restored after read").modified,
        paths(&["src/a.ts"])
    );
}

/// A file the scan never saw — briefly gone, or briefly excluded — reaches
/// `dirty` through the fresh-scan comparison.
#[test]
fn a_file_missing_from_the_scan_and_then_restored_stays_dirty() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    for exclude in [false, true] {
        let repo = Repo::new(if exclude { "race-ignore" } else { "race-gone" });
        repo.write("src/a.ts", "export const a = 1;\n");
        repo.write("src/z.ts", "export const z = 1;\n");
        repo.commit("init");
        let root = repo.root.clone();
        git_pending_hooks::set_after_git_begin(Some(Box::new(move |_| {
            if exclude {
                fs::write(root.join(".gitignore"), "src/z.ts\n").unwrap();
            } else {
                fs::rename(root.join("src/z.ts"), root.join("z.ts.away")).unwrap();
            }
        })));
        let root = repo.root.clone();
        git_pending_hooks::set_before_git_record(Some(Box::new(move |_| {
            if exclude {
                fs::remove_file(root.join(".gitignore")).unwrap();
            } else {
                fs::rename(root.join("z.ts.away"), root.join("src/z.ts")).unwrap();
            }
        })));
        repo.sync();
        git_pending_hooks::set_after_git_begin(None);
        git_pending_hooks::set_before_git_record(None);
        assert!(
            repo.dirty().contains("src/z.ts"),
            "{exclude}: {:?}",
            repo.dirty()
        );
        assert_eq!(
            repo.assert_fast("omitted then restored").added,
            paths(&["src/z.ts"]),
            "exclude={exclude}"
        );
    }
}

/// A full sync forgets the record before any row moves, so an interrupted
/// one can never leave a stale record behind.
#[test]
fn a_full_sync_forgets_the_record_before_it_scans() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("forget") else {
        return;
    };
    assert!(repo.record().is_some());
    let db = IndexPaths::resolve(&repo.root, codegraph_dir().as_deref())
        .unwrap()
        .current_db();
    let absent = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&absent);
    git_pending_hooks::set_after_git_begin(Some(Box::new(move |_| {
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let present: Option<String> = conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = ?1",
                [GIT_PENDING_KEY],
                |row| row.get(0),
            )
            .ok();
        seen.store(present.is_none(), Ordering::SeqCst);
    })));
    repo.sync();
    git_pending_hooks::set_after_git_begin(None);
    assert!(
        absent.load(Ordering::SeqCst),
        "the record was gone during the sync"
    );
    assert!(repo.record().is_some(), "and written again at its end");
}

/// `CODEGRAPH_DIR` names a single project-local index root. The watcher's
/// policy only knows `.codegraph*`, but the scan prunes the exact root, so
/// the index's own files never become candidates.
#[test]
fn a_custom_index_root_is_excluded_by_the_scan_rules() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let _dir = IndexDirGuard::set("cgidx");
    let repo = Repo::new("custom-root");
    fs::write(repo.top.join(".git/info/exclude"), "").unwrap();
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.commit("init");
    repo.sync();
    assert!(
        repo.root.join("cgidx").is_dir(),
        "the index lives in the custom root"
    );
    assert!(repo.assert_fast("custom root, clean").is_empty());
    repo.write("src/a.ts", "export const a = 2;\n");
    assert_eq!(
        repo.assert_fast("custom root, an edit").modified,
        paths(&["src/a.ts"])
    );
}

#[test]
fn a_submodule_below_a_subdirectory_project_declines() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let source = Repo::new("subdir-submodule-source");
    source.write("lib.ts", "export const lib = 1;\n");
    source.commit("lib");
    let repo = Repo::in_subdir("subdir-submodule");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.commit("init");
    repo.git(&[
        "submodule",
        "add",
        "-q",
        &git_path(&source.top),
        "packages/app/vendor",
    ]);
    repo.commit("submodule");
    repo.sync();
    assert_eq!(
        repo.record().expect("a record")["submodules"],
        serde_json::json!(true),
        "the --full-name gitlink check sees a submodule below the project"
    );
    repo.assert_declines("a submodule below a subdirectory project");
}

/// A full sync that dies after forgetting the record and before writing the
/// new one leaves no record, never a stale one.
#[test]
fn an_interrupted_full_sync_leaves_no_record() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("interrupted-full") else {
        return;
    };
    assert!(raw_record(&repo.root).is_some());
    repo.write("src/a.ts", "export const a = 8;\n");
    git_pending_hooks::set_before_git_record(Some(Box::new(|_| {
        panic!("injected failure before the record is written")
    })));
    let root = repo.root.clone();
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync_project_once(&root)));
    git_pending_hooks::set_before_git_record(None);
    assert!(
        outcome.is_err(),
        "the injected failure interrupted the sync"
    );
    assert!(raw_record(&repo.root).is_none(), "no stale record survives");
}

/// An incremental sync records its paths before any row moves, so one that
/// dies right after still leaves them in `dirty`.
#[test]
fn an_interrupted_incremental_sync_keeps_its_paths_recorded() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("interrupted-incremental") else {
        return;
    };
    repo.write("src/a.ts", "export const a = 9;\n");
    git_pending_hooks::set_after_incremental_extend(Some(Box::new(|_| {
        panic!("injected failure before any row moves")
    })));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repo.sync_paths(&["src/a.ts"])
    }));
    git_pending_hooks::set_after_incremental_extend(None);
    assert!(
        outcome.is_err(),
        "the injected failure interrupted the sync"
    );
    let record = raw_record(&repo.root).expect("the record survives");
    assert!(
        record["dirty"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/a.ts"),
        "{record}"
    );
}

/// Git compares content after line-ending conversion, so a revert can rewrite
/// a file's bytes (LF to CRLF) while `git status` calls it clean. The fast path
/// must decline, and the full inventory reports the changed bytes.
#[test]
fn line_ending_conversion_declines() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("autocrlf") else {
        return;
    };
    repo.git(&["config", "core.autocrlf", "true"]);
    repo.write("src/a.ts", "export const a = 2;\n");
    repo.commit("edit a");
    repo.git(&["revert", "--no-edit", "HEAD"]);
    repo.assert_declines("core.autocrlf");
}

#[test]
fn gitattributes_and_case_insensitive_names_decline() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("attributes") else {
        return;
    };
    repo.write(".gitattributes", "* text=auto\n");
    repo.assert_declines("a .gitattributes file");

    let Some(repo) = indexed("ignorecase") else {
        return;
    };
    repo.git(&["config", "core.ignorecase", "true"]);
    repo.assert_declines("core.ignorecase");
}

/// A global attributes file (the XDG default, as `git var GIT_ATTR_GLOBAL`
/// resolves it) converts content as much as a `.gitattributes` does.
#[test]
fn a_global_attributes_file_declines() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("global-attributes") else {
        return;
    };
    assert!(repo.assert_fast("no attributes anywhere").is_empty());
    let attributes = isolated_config_home().join("git").join("attributes");
    fs::create_dir_all(attributes.parent().unwrap()).unwrap();
    fs::write(&attributes, "* text=auto\n").unwrap();
    let declined = repo.pending().1;
    fs::remove_file(&attributes).unwrap();
    assert_eq!(declined, PendingSource::FullInventory);
}

/// Only the value git uses counts: a global `core.autocrlf=true` that the
/// repository overrides with `false` converts nothing (Git for Windows ships
/// `true` in its system config).
#[test]
fn an_overridden_conversion_setting_keeps_the_fast_path() {
    let _hooks = hooks_guard();
    let Some(repo) = indexed("overridden") else {
        return;
    };
    let global = isolated_config_home().join("gitconfig-autocrlf");
    fs::write(&global, "[core]\n\tautocrlf = true\n\tignorecase = true\n").unwrap();
    let previous = std::env::var_os("GIT_CONFIG_GLOBAL");
    // SAFETY: tests in this binary are serialized by `hooks_guard`.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &global) };
    let (pending, source) = repo.pending();
    // SAFETY: as above.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("GIT_CONFIG_GLOBAL", value),
            None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
        }
    }
    fs::remove_file(&global).ok();
    assert_eq!(source, PendingSource::GitFastPath, "the local false wins");
    assert!(pending.is_empty());
}

/// A `.gitattributes` above a project rooted below its work tree still
/// applies to the project's files.
#[test]
fn an_ancestor_gitattributes_above_a_subdirectory_project_declines() {
    let _hooks = hooks_guard();
    if !git_available() {
        return;
    }
    let repo = Repo::in_subdir("ancestor-attributes");
    repo.write("src/a.ts", "export const a = 1;\n");
    repo.commit("init");
    repo.sync();
    assert!(repo.assert_fast("no attributes yet").is_empty());
    for (dir, label) in [
        (repo.top.clone(), "the work-tree root"),
        (repo.top.join("packages"), "an intermediate directory"),
    ] {
        let attributes = dir.join(".gitattributes");
        fs::write(&attributes, "* text=auto\n").unwrap();
        repo.assert_declines(label);
        fs::remove_file(&attributes).unwrap();
    }
    assert!(repo.assert_fast("attributes removed again").is_empty());
}
