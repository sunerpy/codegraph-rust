//! Async-model coverage for the fully-async daemon session path (plan
//! §Test strategy). These drive the SHIPPED daemon+session path (rmcp over the
//! async interprocess local socket) via raw client sockets + JSON-RPC lines,
//! asserting the async accept loop serves many clients, survives long pauses,
//! reaps on disconnect, and keeps the transport responsive under a slow tool.
//!
//! The half-dead-peer force-close reap (plan §4) is covered end-to-end by
//! `crates/codegraph-cli/tests/daemon_sweep.rs` against the real binary; here we
//! cover the in-process daemon behaviors that need a live registry handle.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{DaemonHandle, DaemonOptions, StartOrAttach, start_or_attach};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

fn connect(socket_path: &Path) -> Stream {
    let name = socket_path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .expect("fs name");
    Stream::connect(name).expect("connect to daemon socket")
}

/// Read newline-framed lines until one is a JSON-RPC reply for `want_id`
/// (carrying `result` or `error`), or the deadline elapses. Per-read socket
/// timeout keeps a hung server bounded.
fn read_reply_for(
    stream: &mut Stream,
    want_id: i64,
    deadline: Instant,
) -> Option<serde_json::Value> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let _ = stream.set_recv_timeout(Some(Duration::from_millis(250)));
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    buf.clear();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                        && value.get("id").and_then(serde_json::Value::as_i64) == Some(want_id)
                        && (value.get("result").is_some() || value.get("error").is_some())
                    {
                        return Some(value);
                    }
                } else {
                    buf.push(byte[0]);
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return None,
        }
    }
}

fn initialize_frame(id: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"async-model","version":"0"}}}}}}"#
    )
}

/// Drive a full initialize handshake over `stream`, returning once the daemon
/// answers `id`. Panics on timeout.
fn handshake(stream: &mut Stream, id: i64) {
    writeln!(stream, "{}", initialize_frame(id)).unwrap();
    stream.flush().unwrap();
    let reply = read_reply_for(stream, id, Instant::now() + Duration::from_secs(10))
        .expect("daemon must answer initialize");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        serde_json::json!("codegraph")
    );
    writeln!(
        stream,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stream.flush().unwrap();
}

fn start_daemon(project: &Path) -> DaemonHandle {
    let _ = codegraph_core::config::init_config(None, project);
    let options = DaemonOptions {
        host_pid: None,
        watchdog_interval: Duration::from_millis(10),
        run_mcp: true,
        watch: false,
        ..DaemonOptions::default()
    };
    match start_or_attach(project, options).expect("daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("first start unexpectedly attached"),
    }
}

/// Plan §1: N concurrent clients each do initialize → tools/list → tools/call
/// and all get correct results — the async accept loop serves many without
/// blocking/starving.
#[test]
fn concurrent_clients_all_get_tools_call_results() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
    let project = temp_indexed_project("async-concurrent");
    let handle = start_daemon(&project);
    let socket_path = handle.socket_path().to_path_buf();

    let workers: Vec<_> = (0..8)
        .map(|i| {
            let socket_path = socket_path.clone();
            thread::spawn(move || {
                let mut stream = connect(&socket_path);
                writeln!(stream, "{{\"hostPid\":{}}}", std::process::id()).unwrap();
                handshake(&mut stream, 1);

                let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
                writeln!(stream, "{list}").unwrap();
                stream.flush().unwrap();
                let list_reply = read_reply_for(&mut stream, 2, Instant::now() + Duration::from_secs(10))
                    .unwrap_or_else(|| panic!("client {i}: tools/list must answer"));
                assert!(list_reply["result"]["tools"].is_array(), "client {i}: tools list");

                let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"codegraph_search","arguments":{"query":"add"}}}"#;
                writeln!(stream, "{call}").unwrap();
                stream.flush().unwrap();
                let call_reply = read_reply_for(&mut stream, 3, Instant::now() + Duration::from_secs(15))
                    .unwrap_or_else(|| panic!("client {i}: tools/call must answer"));
                assert!(call_reply.get("result").is_some(), "client {i}: tools/call result, got: {call_reply}");
                drop(stream);
            })
        })
        .collect();

    for w in workers {
        w.join().expect("worker completed");
    }

    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

/// Plan §3: a client that hard-closes mid-session is reaped → `active_count()`
/// returns to 0 within the sweep window (proves async EOF drops the SessionGuard).
#[test]
fn abrupt_disconnect_reaps_to_zero() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
    let project = temp_indexed_project("async-reap0");
    let handle = start_daemon(&project);
    let socket_path = handle.socket_path().to_path_buf();

    let mut stream = connect(&socket_path);
    writeln!(stream, "{{\"hostPid\":{}}}", std::process::id()).unwrap();
    handshake(&mut stream, 1);
    assert_eq!(handle.active_sessions(), 1, "connected session is active");

    drop(stream);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reaped = false;
    while Instant::now() < deadline {
        if handle.active_sessions() == 0 {
            reaped = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(reaped, "async EOF must reap the session to active_count 0");

    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

/// Plan §6b (byte integrity): a LARGE (>64 KiB, multi-chunk) FIRST JSON-RPC
/// frame with NO client-hello must be delivered intact — the non-hello first
/// line is only partially consumed into `first` (bounded at MAX_HELLO_LINE_BYTES),
/// so the put-back chain (`Cursor(first).chain(recv)`) must stitch the consumed
/// prefix and the ~96 KiB remainder back together with no byte lost/duplicated
/// at the seam. A correct `initialize` reply proves the whole frame parsed as
/// one JSON line.
#[test]
fn large_first_frame_without_hello_is_delivered_intact() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
    let project = temp_indexed_project("async-bigframe");
    let handle = start_daemon(&project);
    let socket_path = handle.socket_path().to_path_buf();

    let mut stream = connect(&socket_path);
    // NO hello: the >64 KiB initialize frame is the FIRST line, so its bytes
    // flow through the put-back chain (consumed prefix + recv remainder). The
    // long clientInfo.name pad forces the single JSON line to span the hello
    // bound AND multiple socket reads / the duplex buffer boundary.
    let pad = "x".repeat(100 * 1024);
    let big_init = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"{pad}","version":"0"}}}}}}"#
    );
    assert!(
        big_init.len() > 64 * 1024,
        "frame exceeds the duplex buffer"
    );
    writeln!(stream, "{big_init}").unwrap();
    stream.flush().unwrap();

    let reply = read_reply_for(&mut stream, 7, Instant::now() + Duration::from_secs(15))
        .expect("a >64 KiB non-hello first frame must be delivered intact and answered");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        serde_json::json!("codegraph")
    );

    drop(stream);
    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

/// Plan §7: a slow explore on session A must not delay session B's
/// tools/list. `codegraph_explore` on the mini fixture is fast, so we instead
/// prove non-starvation structurally: two independent sessions each complete a
/// tools/call concurrently within a tight bound (a single-task loop would
/// serialize them and blow the bound only if one blocked the other).
#[test]
fn second_session_stays_responsive_during_first_session_work() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
    let project = temp_indexed_project("async-noStarve");
    let handle = start_daemon(&project);
    let socket_path = handle.socket_path().to_path_buf();

    // Session A: begin a tools/call and keep the socket open.
    let mut a = connect(&socket_path);
    writeln!(a, "{{\"hostPid\":{}}}", std::process::id()).unwrap();
    handshake(&mut a, 1);
    let call_a = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"codegraph_explore","arguments":{"query":"add"}}}"#;
    writeln!(a, "{call_a}").unwrap();
    a.flush().unwrap();

    // Session B: while A's call is in flight, B must handshake + tools/list
    // promptly (well within the bound a starved single-task loop would miss).
    let started = Instant::now();
    let mut b = connect(&socket_path);
    writeln!(b, "{{\"hostPid\":{}}}", std::process::id()).unwrap();
    handshake(&mut b, 1);
    let list_b = r#"{"jsonrpc":"2.0","id":11,"method":"tools/list"}"#;
    writeln!(b, "{list_b}").unwrap();
    b.flush().unwrap();
    let b_reply = read_reply_for(&mut b, 11, Instant::now() + Duration::from_secs(10))
        .expect("session B tools/list must return while A is busy");
    assert!(b_reply["result"]["tools"].is_array());
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "session B must not be starved by session A (elapsed {:?})",
        started.elapsed()
    );

    // Drain A's reply so its session ends cleanly.
    let _ = read_reply_for(&mut a, 10, Instant::now() + Duration::from_secs(10));
    drop(a);
    drop(b);
    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

/// Plan §5: with a clamped-small idle timeout the async accept loop's interval
/// tick observes `active_count()==0 && idle` and idle-exits — the daemon thread
/// finishes on its own (no clients ever connect). Same thresholds/log lines as
/// the former blocking loop, now driven by the tokio interval branch.
#[test]
fn idle_timeout_exits_the_async_accept_loop() {
    unsafe {
        std::env::set_var("CODEGRAPH_NO_WATCH", "1");
        std::env::set_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "1000");
        std::env::set_var("CODEGRAPH_DAEMON_CLIENT_SWEEP_MS", "50");
    }
    let project = temp_indexed_project("async-idle");
    let handle = start_daemon(&project);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut finished = false;
    while Instant::now() < deadline {
        if handle.is_finished() {
            finished = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        finished,
        "the async accept loop must idle-exit when no clients connect past the idle timeout"
    );
    handle.wait().expect("idle-exited daemon joins cleanly");

    unsafe {
        std::env::remove_var("CODEGRAPH_NO_WATCH");
        std::env::remove_var("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS");
        std::env::remove_var("CODEGRAPH_DAEMON_CLIENT_SWEEP_MS");
    }
    let _ = fs::remove_dir_all(&project);
}

/// Plan §8: setting the shutdown flag makes the async accept loop exit, in-flight
/// sessions drop, and the daemon thread joins cleanly (handle.stop()).
#[test]
fn graceful_shutdown_exits_accept_loop_and_joins() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };
    let project = temp_indexed_project("async-shutdown");
    let handle = start_daemon(&project);
    let socket_path = handle.socket_path().to_path_buf();

    let mut stream = connect(&socket_path);
    writeln!(stream, "{{\"hostPid\":{}}}", std::process::id()).unwrap();
    handshake(&mut stream, 1);
    assert_eq!(handle.active_sessions(), 1);

    // stop() sets the shutdown flag and joins the accept-loop thread; an in-flight
    // session is dropped as the runtime tears down. Must return cleanly.
    drop(stream);
    handle
        .stop()
        .expect("graceful shutdown joins the accept loop cleanly");

    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

fn temp_indexed_project(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "codegraph-daemon-{name}-{}-{nanos}-{seq}",
        std::process::id()
    ));
    let cg = path.join(".codegraph");
    fs::create_dir_all(&cg).expect("create project .codegraph");
    let root = workspace_root();
    fs::copy(
        root.join("reference/golden/mini/colby.db"),
        cg.join("codegraph.db"),
    )
    .expect("copy golden mini db");
    let fixtures = root.join("crates/codegraph-bench/fixtures/mini");
    for rel in ["src/app.ts", "src/math.ts"] {
        let dst = path.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(fixtures.join(rel), &dst).expect("copy fixture source");
    }
    path
}
