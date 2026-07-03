//! Phase D / Decision 12 — the daemon dead-client reap contract MUST survive
//! the rmcp-backed session.
//!
//! The daemon serves each connection on a per-connection blocking `std::thread`
//! (`lib.rs` accept loop → `serve_session`). `shutdown_session` reaps a wedged
//! client by half/full-closing its socket so the session thread hits EOF,
//! returns, and drops its `SessionGuard` (which removes the registry entry).
//!
//! Phase A/B/C moved the transport to rmcp, whose async stdio serve runs on a
//! `tokio` CURRENT-THREAD runtime INSIDE that blocking thread (Decision B5).
//! This test proves the async-rmcp session STILL honors force-EOF reap: after a
//! socket half-close, the session terminates and the registry drops to zero
//! within a bounded timeout — the same contract `read_daemon_hello_times_out`
//! does NOT cover.
//!
//! Gated on `--features rmcp` because it specifically exercises the
//! rmcp-backed session path (routed when `CODEGRAPH_DAEMON_RMCP=1`).

#![cfg(all(unix, feature = "rmcp"))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{DaemonOptions, StartOrAttach, start_or_attach};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

/// Connect a fresh blocking client stream to the daemon socket.
fn connect(socket_path: &Path) -> Stream {
    let name = socket_path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .expect("fs name");
    Stream::connect(name).expect("connect to daemon socket")
}

/// Read newline-framed lines from `reader` until one parses as a JSON-RPC reply
/// carrying `id == 1` (the initialize response), or the deadline elapses.
fn read_initialize_reply<R: BufRead>(
    reader: &mut R,
    deadline: Instant,
) -> Option<serde_json::Value> {
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    // Skip the daemon hello (has `codegraph`, not `id`).
                    if value.get("id").and_then(serde_json::Value::as_i64) == Some(1)
                        && value.get("result").is_some()
                    {
                        return Some(value);
                    }
                }
            }
            Err(_) => return None,
        }
    }
    None
}

#[test]
fn shutdown_reaps_rmcp_backed_session_on_socket_close() {
    // Route the daemon session through the rmcp serve path.
    unsafe { std::env::set_var("CODEGRAPH_DAEMON_RMCP", "1") };
    // Keep the daemon from spawning a watcher/catch-up that would touch the
    // empty temp project (run_mcp still true so the session actually serves).
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };

    let project = temp_project("rmcp-reap");
    let options = DaemonOptions {
        host_pid: None,
        watchdog_interval: Duration::from_millis(10),
        run_mcp: true,
        watch: false,
        ..DaemonOptions::default()
    };

    let handle = match start_or_attach(&project, options).expect("daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("first start unexpectedly attached"),
    };
    let socket_path = handle.socket_path().to_path_buf();

    // Connect and drive a full initialize through the rmcp-backed session. A
    // non-empty initialize reply proves the rmcp handler is serving this socket.
    let mut stream = connect(&socket_path);
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"reap-test","version":"0"}}}"#;
    writeln!(stream, "{init}").expect("write initialize");
    stream.flush().expect("flush initialize");

    let mut reader = BufReader::new(&mut stream);
    let deadline = Instant::now() + Duration::from_secs(10);
    let reply = read_initialize_reply(&mut reader, deadline)
        .expect("rmcp-backed session must answer initialize with a result");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        serde_json::json!("codegraph"),
        "the rmcp handler must identify as codegraph"
    );

    // The session is registered and serving.
    assert_eq!(
        handle.active_sessions(),
        1,
        "the connected rmcp session must be registered as active"
    );

    // Force-EOF the session by fully closing our socket (the client side). The
    // daemon's session reader (rmcp async serve on a current-thread runtime)
    // must see stream-end, return from block_on, and drop its SessionGuard.
    drop(reader);
    drop(stream);

    // Assert the reap happens within a bounded timeout.
    let reap_deadline = Instant::now() + Duration::from_secs(10);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if handle.active_sessions() == 0 {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        reaped,
        "the rmcp-backed session thread must terminate + be reaped after socket close \
         (active_sessions never returned to 0)"
    );

    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_DAEMON_RMCP") };
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(project);
}

static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_project(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "codegraph-daemon-{name}-{}-{nanos}-{seq}",
        std::process::id()
    ));
    fs::create_dir_all(path.join(".codegraph")).expect("create project");
    path
}
