//! Fix A — daemon stale-socket robustness (rmcp-migration).
//!
//! Root-caused production bug: a stdio `serve --mcp` on a project whose daemon
//! DIED but left a stale `daemon.sock` behind hung until the MCP client timed
//! out. `daemon_already_running` correctly saw the pid as dead and spawned a
//! fresh daemon, but the residual failure was the attach path connecting to the
//! recorded socket (`socket_path.exists()` true) and reading the daemon hello
//! with NO bound — a dead/half-open/orphaned socket blocked forever.
//!
//! These tests pin the two guarantees:
//!   1. `attach_to_daemon` against a stale/orphaned unix socket RETURNS an Err
//!      within a bounded time (never hangs).
//!   2. The stale-socket self-heal helper clears the leftover `daemon.sock` +
//!      `daemon.pid` when the recorded pid is dead, so the NEXT startup spawns a
//!      fresh daemon cleanly.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{
    DaemonLockInfo, attach_to_daemon, clear_stale_daemon_socket, daemon_pid_path, encode_lock_info,
    recorded_socket_path,
};

// A pid that is not a live process on any sane Unix host.
const DEAD_PID: u32 = 999_999_999;
// Generous wall-clock ceiling: comfortably above the daemon hello timeout
// (2s) so a healthy machine never flakes, while still failing fast if the
// attach path ignores its bound entirely (the old behavior hung forever).
const ATTACH_CEILING: Duration = Duration::from_secs(4);

/// 1. A stale unix socket FILE (created by a `UnixListener` that was then
///    dropped, leaving the filesystem entry with nothing accepting) must not
///    hang the attach: `attach_to_daemon` returns an Err within the bound.
#[test]
fn attach_to_orphaned_socket_returns_err_within_bound() {
    let project = temp_project("orphaned-sock");
    let socket_path = project.join(".codegraph/daemon.sock");

    // Bind a listener to create the .sock file, then DROP it: the filesystem
    // entry survives but nothing is accepting. A connect either refuses or
    // (on some kernels) succeeds-then-EOFs — in every case the attach MUST NOT
    // block waiting for a hello.
    {
        let listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind stale unix listener");
        drop(listener);
    }
    assert!(
        socket_path.exists(),
        "the orphaned .sock file must still be present (stale artifact)"
    );

    let started = Instant::now();
    let result = attach_to_daemon(&socket_path);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "attaching to an orphaned socket (no live daemon) must return Err, not a client"
    );
    assert!(
        elapsed < ATTACH_CEILING,
        "attach to a stale socket must be bounded, not hang (elapsed {elapsed:?})"
    );

    let _ = fs::remove_dir_all(project);
}

/// 1b. A plain FILE at the `.sock` path (not even a socket) must also fail fast
///     — this is the `touch .codegraph/daemon.sock` shape of the leftover.
#[test]
fn attach_to_plain_file_socket_returns_err_within_bound() {
    let project = temp_project("plain-file-sock");
    let socket_path = project.join(".codegraph/daemon.sock");
    fs::write(&socket_path, b"not a socket").expect("write plain file at sock path");

    let started = Instant::now();
    let result = attach_to_daemon(&socket_path);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "attaching to a plain file at the socket path must return Err"
    );
    assert!(
        elapsed < ATTACH_CEILING,
        "attach to a non-socket file must be bounded, not hang (elapsed {elapsed:?})"
    );

    let _ = fs::remove_dir_all(project);
}

/// 2. Stale self-heal: given a project with a stale `daemon.pid` (dead pid) +
///    a leftover `daemon.sock`, `clear_stale_daemon_socket` removes BOTH so a
///    subsequent spawn starts fresh. It must return `true` (healed).
#[test]
fn clear_stale_daemon_socket_removes_dead_pid_sock_and_lock() {
    let project = temp_project("self-heal-dead");
    let pid_path = daemon_pid_path(&project);
    let socket_path = project.join(".codegraph/daemon.sock");

    write_lock(&pid_path, DEAD_PID, &socket_path);
    // Leave an orphaned socket file behind (the crashed daemon's residue).
    {
        let listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind stale unix listener");
        drop(listener);
    }
    assert!(pid_path.exists(), "precondition: stale pid file present");
    assert!(
        socket_path.exists(),
        "precondition: stale sock file present"
    );

    let healed = clear_stale_daemon_socket(&project);

    assert!(
        healed,
        "a stale sock + dead-pid lock must be healed (report true)"
    );
    assert!(
        !pid_path.exists(),
        "the stale daemon.pid must be removed by self-heal"
    );
    assert!(
        !socket_path.exists(),
        "the stale daemon.sock must be removed by self-heal"
    );

    let _ = fs::remove_dir_all(project);
}

/// 2b. Self-heal MUST NOT touch a lock held by a LIVE pid: if the recorded pid
///     is alive (our own process), the sock/pid are left intact and it reports
///     `false` (not healed).
#[test]
fn clear_stale_daemon_socket_preserves_live_pid_lock() {
    let project = temp_project("self-heal-live");
    let pid_path = daemon_pid_path(&project);
    let socket_path = project.join(".codegraph/daemon.sock");

    // Our own pid is alive — a live lock must never be cleared.
    write_lock(&pid_path, process::id(), &socket_path);
    {
        let listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind unix listener");
        drop(listener);
    }

    let healed = clear_stale_daemon_socket(&project);

    assert!(!healed, "a live-pid lock must NOT be healed (report false)");
    assert!(
        pid_path.exists(),
        "a live-pid lock's pid file must be preserved"
    );

    let _ = fs::remove_dir_all(project);
}

/// 2c. Self-heal against the RECORDED socket: the socket removed is the one the
///     lock recorded (fallback-aware), not merely the default path. Proves the
///     helper reads `recorded_socket_path` rather than recomputing.
#[test]
fn clear_stale_daemon_socket_removes_recorded_fallback_socket() {
    let project = temp_project("self-heal-recorded");
    let pid_path = daemon_pid_path(&project);
    // Record a DIFFERENT socket path than the default (a fallback-tmpdir shape).
    let recorded = project.join(".codegraph/daemon-fallback.sock");
    write_lock(&pid_path, DEAD_PID, &recorded);
    {
        let listener =
            std::os::unix::net::UnixListener::bind(&recorded).expect("bind recorded sock");
        drop(listener);
    }
    assert_eq!(
        recorded_socket_path(&project),
        recorded,
        "precondition: lock records the fallback socket"
    );

    let healed = clear_stale_daemon_socket(&project);

    assert!(healed, "stale recorded socket + dead pid must heal");
    assert!(!recorded.exists(), "the RECORDED socket must be removed");
    assert!(!pid_path.exists(), "the stale pid must be removed");

    let _ = fs::remove_dir_all(project);
}

fn write_lock(pid_path: &Path, pid: u32, socket_path: &Path) {
    let info = DaemonLockInfo {
        pid,
        version: "test".to_string(),
        socket_path: socket_path.to_path_buf(),
        started_at: 1,
    };
    fs::create_dir_all(pid_path.parent().expect("pid parent")).expect("create .codegraph");
    fs::write(pid_path, encode_lock_info(&info).expect("serialize lock")).expect("write lock");
}

fn temp_project(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "codegraph-daemon-stale-{name}-{}-{nanos}",
        process::id()
    ));
    fs::create_dir_all(path.join(".codegraph")).expect("create project");
    path
}
