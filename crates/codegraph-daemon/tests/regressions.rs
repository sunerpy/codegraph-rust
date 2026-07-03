//! Regression tests for the three daemon primitives that the C1 transport
//! refactor changed, so a later edit cannot silently revert them:
//!   1. lock contention -> exactly one winner (atomic create_new + write-then-
//!      publish claim);
//!   2. shutdown via the AtomicBool flag stops the nonblocking accept loop
//!      (the C1 replacement for the old self-connect poke);
//!   3. stale-lock recovery skips/clears a dead pid, and a reader never deletes
//!      an EMPTY in-flight pidfile (audit BLOCKER #6).
//!
//! These are behavioral assertions: reverting the C1 behavior turns them red.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{
    AcquireResult, DaemonHandle, DaemonLockInfo, DaemonOptions, StartOrAttach,
    clear_stale_daemon_lock, daemon_pid_path, encode_lock_info, start_or_attach,
    try_acquire_daemon_lock,
};

// A pid that is not a live process on any sane Unix host; used to forge a
// "stale" lock whose owner is provably dead.
const DEAD_PID: u32 = 999_999_999;
// Generous ceiling for the bounded waits below: comfortably above the 250ms
// ACCEPT_POLL_INTERVAL so a healthy machine never flakes, while still failing
// fast if the accept loop ignores the shutdown flag entirely.
const SHUTDOWN_CEILING: Duration = Duration::from_secs(5);
// Env var that re-execs this test binary into "lock claimer" child mode; its
// value is the project root the child races for.
const CLAIM_CHILD_ENV: &str = "CODEGRAPH_REG_CLAIM_PROJECT";
// Distinct child exit codes the parent decodes into winner/loser tallies; kept
// off 0/1 so a normal exit or panic cannot be miscounted as an outcome.
const CHILD_ACQUIRED: i32 = 70;
const CHILD_TAKEN: i32 = 71;
const CONTENDER_PROCS: usize = 8;

// 1. Lock contention: N separate PROCESSES race to acquire the lock on a single
//    fresh project root; exactly one must win, the rest must lose cleanly --
//    never two winners, never a corrupted/empty published pidfile. Reverting the
//    atomic create_new + write-then-publish claim turns this red.
//
//    Single-instance locking is a process-level contract: each contender must
//    own a distinct pid so the pid-keyed temp file and liveness checks behave as
//    in production. Eight same-process threads would instead collide on the
//    shared `daemon.pid.<pid>.tmp` and model a scenario that cannot occur with
//    real daemon processes, so this test re-execs the binary per contender.
#[test]
fn concurrent_acquire_has_exactly_one_process_winner() {
    run_claim_child_if_requested();

    let project = temp_project("lock-contention");
    let exe = std::env::current_exe().expect("locate test binary");

    let mut children = Vec::with_capacity(CONTENDER_PROCS);
    for _ in 0..CONTENDER_PROCS {
        let child = Command::new(&exe)
            .arg("--exact")
            .arg("concurrent_acquire_has_exactly_one_process_winner")
            .env(CLAIM_CHILD_ENV, &project)
            .spawn()
            .expect("spawn contender process");
        children.push(child);
    }

    let mut acquired = 0usize;
    let mut taken = 0usize;
    for mut child in children {
        let status = child.wait().expect("contender exits");
        match status.code() {
            Some(CHILD_ACQUIRED) => acquired += 1,
            Some(CHILD_TAKEN) => taken += 1,
            other => panic!("contender exited with unexpected status {other:?}"),
        }
    }

    assert_eq!(
        acquired, 1,
        "exactly one process must win the lock (got {acquired} winners, {taken} taken)"
    );
    assert_eq!(
        taken,
        CONTENDER_PROCS - 1,
        "every non-winner must observe the lock as taken"
    );

    // The published pidfile is a full, valid lock record -- never the empty
    // create_new placeholder a torn write-then-publish would leave behind.
    let pid_path = daemon_pid_path(&project);
    let raw = fs::read_to_string(&pid_path).expect("winner published a pidfile");
    assert!(
        !raw.trim().is_empty(),
        "the published pidfile must not be an empty placeholder"
    );
    let info: DaemonLockInfo =
        serde_json::from_str(raw.trim()).expect("pidfile is valid lock JSON");
    assert!(
        info.pid > 0,
        "the published lock must name a real winning pid, not a corrupted record"
    );

    // The contender children are short-lived claimers (no accept loop), so their
    // pids are dead by now; clear the lock they left and remove the temp tree.
    assert!(
        clear_stale_daemon_lock(&pid_path, None),
        "the dead-child lock must clear after every contender exits"
    );
    let _ = fs::remove_dir_all(project);
}

// 2. Shutdown via the flag stops the accept loop: this pins the C1 change from
//    self-connect-poke to nonblocking-accept + poll. A running daemon must
//    terminate its accept thread promptly after stop() and clean the pidfile.
#[test]
fn stop_flag_terminates_accept_loop_and_clears_pidfile() {
    let project = temp_project("shutdown-flag");

    let handle = match start_or_attach(&project, test_options()).expect("daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("first start unexpectedly attached"),
    };
    assert!(
        daemon_pid_path(&project).exists(),
        "a started daemon must publish its pidfile"
    );
    assert!(
        !handle.is_finished(),
        "the accept loop must still be running before stop()"
    );

    // stop() flips the shutdown AtomicBool and joins the accept thread. With the
    // poll loop this returns only once the thread observes the flag, so a bounded
    // timeout around the join proves promptness (a hung loop blocks the join).
    let elapsed = stop_within(handle, SHUTDOWN_CEILING);
    assert!(
        elapsed < SHUTDOWN_CEILING,
        "accept loop must stop within {SHUTDOWN_CEILING:?}, took {elapsed:?}"
    );
    assert!(
        !daemon_pid_path(&project).exists(),
        "stopping the daemon must clean up its pidfile"
    );
    let _ = fs::remove_dir_all(project);
}

// 3a. Stale-lock recovery: a pidfile whose owner is dead must be recoverable --
//     start_or_attach clears the stale lock and starts a fresh daemon.
#[test]
fn dead_pid_lock_is_recovered_by_start_or_attach() {
    let project = temp_project("stale-dead-pid");
    let pid_path = daemon_pid_path(&project);
    write_lock(&pid_path, DEAD_PID);

    let handle = match start_or_attach(&project, test_options()).expect("recovers stale lock") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("stale lock should not be attachable"),
    };
    let raw = fs::read_to_string(&pid_path).expect("read republished pidfile");
    let info: DaemonLockInfo = serde_json::from_str(raw.trim()).expect("lock JSON");
    assert_eq!(
        info.pid,
        std::process::id(),
        "the stale lock must be replaced by this live process"
    );

    handle.stop().expect("daemon stops");
    assert!(!pid_path.exists());
    let _ = fs::remove_dir_all(project);
}

// 3a (direct). clear_stale_daemon_lock removes a lock owned by a dead pid.
#[test]
fn clear_stale_daemon_lock_removes_dead_pid_lock() {
    let project = temp_project("clear-dead-pid");
    let pid_path = daemon_pid_path(&project);
    write_lock(&pid_path, DEAD_PID);

    assert!(
        clear_stale_daemon_lock(&pid_path, Some(DEAD_PID)),
        "a dead-pid lock must clear"
    );
    assert!(!pid_path.exists(), "the stale lock file must be removed");
    let _ = fs::remove_dir_all(project);
}

// 3b. Empty pidfile is NOT deleted: an empty (0-byte) pidfile simulates an
//     in-flight publish (create_new placeholder before the rename lands). A
//     reader must treat it as live and never delete it -- this pins audit
//     BLOCKER #6. Reverting to delete-on-empty turns this red.
#[test]
fn empty_pidfile_is_not_deleted_by_reader() {
    let project = temp_project("empty-pidfile");
    let pid_path = daemon_pid_path(&project);

    // Durable 0-byte placeholder: `fs::write` can return before the directory
    // entry is flushed, so under load the guard's 20ms-later re-read
    // (EMPTY_RETRY_DELAY in read_pidfile_tolerant) could race a not-yet-durable
    // write. sync_all() forces it to disk first, so the SUT always re-reads a
    // present empty file -- killing the load sensitivity, not what is proven.
    write_empty_pidfile_durably(&pid_path);
    assert!(
        pid_path.exists(),
        "the durably-synced empty pidfile must be present before the reader runs"
    );
    assert_eq!(
        fs::metadata(&pid_path).expect("stat pidfile").len(),
        0,
        "the pidfile must start empty (0 bytes)"
    );

    // Hammer the guard 50x: a transient load-induced misread, if one were
    // possible, surfaces here deterministically instead of as a rare CI flake.
    for attempt in 0..50 {
        let cleared = clear_stale_daemon_lock(&pid_path, None);
        assert!(
            !cleared,
            "attempt {attempt}: an empty in-flight pidfile must report not-cleared, never deleted"
        );
        assert!(
            pid_path.exists(),
            "attempt {attempt}: an empty in-flight pidfile must survive a reader (in-flight, not stale)"
        );
    }

    // This test deliberately leaves an undeleted placeholder; drop it before the
    // tree teardown so cleanup is deterministic and never leaks the dir.
    let _ = fs::remove_file(&pid_path);
    let _ = fs::remove_dir_all(project);
}

// fsync a 0-byte file so its directory entry is durable before the caller reads
// it. Plain `fs::write(path, b"")` can return pre-flush; under load that lets the
// guard's 20ms retry window re-read a transient state.
fn write_empty_pidfile_durably(pid_path: &Path) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(pid_path)
        .expect("open empty pidfile");
    file.write_all(b"").expect("write empty pidfile");
    file.flush().expect("flush empty pidfile");
    file.sync_all().expect("sync empty pidfile");
    drop(file);
}

// Child entry point: only active in the re-exec'd subprocess (driven by
// CLAIM_CHILD_ENV). Claim the lock once and exit with a code the parent decodes.
fn run_claim_child_if_requested() {
    let Ok(project) = std::env::var(CLAIM_CHILD_ENV) else {
        return;
    };
    let code = match try_acquire_daemon_lock(Path::new(&project)) {
        Ok(AcquireResult::Acquired { .. }) => CHILD_ACQUIRED,
        Ok(AcquireResult::Taken { .. }) => CHILD_TAKEN,
        Err(_) => 1,
    };
    std::process::exit(code);
}

// Join the daemon's accept thread under a wall-clock ceiling, returning how long
// stop() actually took. The join happens on a helper thread so a hung accept
// loop surfaces as an elapsed time at/over the ceiling instead of hanging the
// whole test process forever.
fn stop_within(handle: DaemonHandle, ceiling: Duration) -> Duration {
    let (tx, rx) = mpsc::channel();
    let start = Instant::now();
    thread::spawn(move || {
        let result = handle.stop();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(ceiling) {
        Ok(result) => {
            result.expect("daemon stops cleanly");
            start.elapsed()
        }
        Err(_) => ceiling,
    }
}

fn write_lock(pid_path: &Path, pid: u32) {
    let info = DaemonLockInfo {
        pid,
        version: "test".to_string(),
        socket_path: pid_path.with_file_name("daemon.sock"),
        started_at: 1,
    };
    fs::write(pid_path, encode_lock_info(&info).expect("serialize lock")).expect("write lock");
}

fn test_options() -> DaemonOptions {
    DaemonOptions {
        host_pid: None,
        watchdog_interval: Duration::from_millis(10),
        run_mcp: false,
        ..DaemonOptions::default()
    }
}

// Process-wide monotonic counter guaranteeing every temp_project path is
// globally unique even when two parallel tests call this in the same nanosecond.
// `nanos` alone can collide on coarse-resolution clocks under parallel
// scheduling; a colliding path would let two tests share a project dir, so one
// test's fs::remove_dir_all could delete the other's in-flight pidfile mid-test.
// The atomic suffix makes a collision impossible regardless of clock granularity.
static TEMP_PROJECT_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_project(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let seq = TEMP_PROJECT_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "codegraph-daemon-reg-{name}-{}-{nanos}-{seq}",
        std::process::id()
    ));
    fs::create_dir_all(path.join(".codegraph")).expect("create project");
    path
}
