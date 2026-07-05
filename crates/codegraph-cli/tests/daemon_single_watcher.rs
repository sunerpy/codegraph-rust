//! End-to-end test: the DAEMON process owns ONE `ProjectWatcher` per
//! project, shared by all N client connections.
//!
//! The watcher lives in a SEPARATE detached process, so an in-process
//! `watcher_count()` hook is unreachable. We assert BEHAVIORALLY via the
//! observable single-sync signal the daemon writes to `.codegraph/daemon.log`
//! (its stdout/stderr is redirected there by the T2 detached spawn):
//! `watcher sync #{n}: {files} file(s)`. Exactly ONE such line for a single
//! change proves one watcher fired once, not N (one per connected client).

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

// `std::env` is process-global; both tests mutate `CODEGRAPH_NO_WATCH` and
// `Command` snapshots env only at spawn time. Serialize the set-env → spawn
// region so a parallel test cannot race the inherited env of a detached daemon.
fn env_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

use codegraph_daemon::{
    daemon_socket_path, is_process_alive, spawn_detached_daemon, unlock_project,
};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

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

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-daemon-watcher-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn indexed_project(label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);
    let status = Command::new(bin())
        .args(["init", project.to_str().unwrap()])
        .status()
        .expect("run codegraph init");
    assert!(status.success(), "init failed for {}", project.display());
    (dir, project)
}

/// Open a client connection to the daemon, read+discard its hello line, and
/// return the live stream so the connection stays open (one of the N clients).
fn connect_client(socket: &Path) -> Option<Stream> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    // Read and discard the daemon hello line so the connection is fully
    // established as a real client.
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    Some(stream)
}

/// Read the daemon pid from a throwaway probe connection's hello line.
fn read_pid_from_hello(socket: &Path) -> Option<u32> {
    let name = socket.to_fs_name::<GenericFilePath>().ok()?;
    let stream = Stream::connect(name).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(|p| p as u32)
}

fn poll_for_daemon_pid(socket: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if socket.exists()
            && let Some(pid) = read_pid_from_hello(socket)
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

// A SIGKILL'd child of this long-lived test harness lingers as a zombie until
// the harness reaps it; signal-0 liveness reports a zombie as "alive". For
// teardown a zombie is functionally dead, so read /proc state and treat `Z`
// (zombie) the same as gone.
fn process_is_gone_or_zombie(pid: u32) -> bool {
    if !is_process_alive(pid) {
        return true;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|state| state == "Z")
            .unwrap_or(false),
        Err(_) => true,
    }
}

fn wait_until_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process_is_gone_or_zombie(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    process_is_gone_or_zombie(pid)
}

fn daemon_log(project: &Path) -> PathBuf {
    project.join(".codegraph").join("daemon.log")
}

/// Read the daemon.log and count the watcher sync lines.
fn count_sync_lines(log: &Path) -> usize {
    let mut contents = String::new();
    if let Ok(mut file) = fs::File::open(log) {
        let _ = file.read_to_string(&mut contents);
    }
    contents
        .lines()
        .filter(|line| line.contains("watcher sync #"))
        .count()
}

/// Return the first watcher sync line in the daemon.log, if any.
fn first_sync_line(log: &Path) -> Option<String> {
    let mut contents = String::new();
    if let Ok(mut file) = fs::File::open(log) {
        let _ = file.read_to_string(&mut contents);
    }
    contents
        .lines()
        .find(|line| line.contains("watcher sync #"))
        .map(str::to_string)
}

/// Poll the daemon.log up to `timeout` for AT LEAST one watcher sync line,
/// returning the final count once at least one appears (or the deadline hits).
fn poll_for_sync_lines(log: &Path, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let count = count_sync_lines(log);
        if count >= 1 || Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// HAPPY: a single source-file change, with TWO clients connected, produces
/// EXACTLY ONE `watcher sync #` line — proving the daemon owns ONE watcher
/// shared by both clients (not one per connection).
#[test]
fn daemon_single_watcher_fires_once() {
    let (_dir, project) = indexed_project("fires-once");
    let socket = daemon_socket_path(&project);
    let log = daemon_log(&project);

    let pid = {
        // Hold the guard across the whole env-set -> spawn -> observe region so
        // the parallel no-watch test cannot toggle CODEGRAPH_NO_WATCH while this
        // daemon is reading its inherited env at startup.
        let _env = env_guard();
        unsafe { std::env::set_var("CODEGRAPH_WATCH_DEBOUNCE_MS", "100") };
        unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
        spawn_detached_daemon(&bin(), &project, false).expect("spawn_detached_daemon");
        poll_for_daemon_pid(&socket, Duration::from_millis(3000))
            .expect("daemon socket + hello pid within poll window")
    };

    // Connect TWO clients (the #411 multi-client case). Hold both open across
    // the file change so any per-connection watcher would double-fire.
    let client_a = connect_client(&socket).expect("first client connects");
    let client_b = connect_client(&socket).expect("second client connects");

    // Write ONE new source file into the project.
    fs::write(
        project.join("brand_new_symbol.ts"),
        "export function brandNewSymbol() { return 411; }\n",
    )
    .unwrap();

    // Wait for the watcher to debounce + sync, polling the log (debounce=100ms
    // + sync time + margin); poll up to 5s so the test is not flaky.
    let count = poll_for_sync_lines(&log, Duration::from_secs(5));

    drop(client_a);
    drop(client_b);

    // TEARDOWN: kill the daemon before asserting so a panicking assert never
    // leaks the process.
    kill_pid(pid);
    let gone = wait_until_gone(pid, Duration::from_secs(5));
    unlock_project(&project);

    assert_eq!(
        count,
        1,
        "expected EXACTLY ONE `watcher sync #` line for a single change \
         (one shared watcher), saw {count}; log:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(gone, "daemon pid {pid} must be dead after teardown");

    // The subscriber prepends an RFC3339 timestamp; the sync event carries the
    // reindexed/removed counts AND the changed filename.
    let sync_line = first_sync_line(&log).expect("a `watcher sync #` line must exist");
    assert!(
        sync_line.contains('T') && sync_line.contains(':'),
        "sync line must carry the subscriber's RFC3339 timestamp: {sync_line:?}"
    );
    assert!(
        sync_line.contains("reindexed") && sync_line.contains("removed"),
        "sync line must show reindexed/removed counts: {sync_line:?}"
    );
    assert!(
        sync_line.contains("brand_new_symbol.ts"),
        "sync line must name the changed file: {sync_line:?}"
    );
}

/// FAILURE: with `CODEGRAPH_NO_WATCH=1` in the daemon's env, the daemon serves
/// but does NOT watch — no `watcher sync #` line ever appears, and the daemon
/// stays alive (does not panic).
#[test]
fn daemon_no_watch_does_not_autosync() {
    let (_dir, project) = indexed_project("no-watch");
    let socket = daemon_socket_path(&project);
    let log = daemon_log(&project);

    let pid = {
        // Hold the guard across env-set -> spawn -> observe; restore the env
        // BEFORE releasing the guard so the happy test's spawn never inherits
        // CODEGRAPH_NO_WATCH.
        let _env = env_guard();
        unsafe { std::env::set_var("CODEGRAPH_WATCH_DEBOUNCE_MS", "100") };
        unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
        let spawn = spawn_detached_daemon(&bin(), &project, true);
        let pid = spawn
            .ok()
            .and_then(|()| poll_for_daemon_pid(&socket, Duration::from_millis(3000)));
        unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
        pid.expect("daemon socket + hello pid within poll window")
    };

    let client = connect_client(&socket).expect("client connects");
    fs::write(
        project.join("should_not_sync.ts"),
        "export function shouldNotSync() { return 0; }\n",
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(100 + 1500));
    let count = count_sync_lines(&log);
    let alive_and_serving = socket.exists() && is_process_alive(pid);

    drop(client);
    kill_pid(pid);
    let gone = wait_until_gone(pid, Duration::from_secs(5));
    unlock_project(&project);

    assert_eq!(
        count,
        0,
        "watching disabled (CODEGRAPH_NO_WATCH=1) must emit NO `watcher sync #` \
         line, saw {count}; log:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        alive_and_serving,
        "daemon must still be alive + serving (socket present) with watching disabled"
    );
    assert!(gone, "daemon pid {pid} must be dead after teardown");
}
