//! Regression: a real MCP client's `tools/call` through the daemon session
//! (rmcp over the blocking `std::io` socket bridge) MUST return a result — it
//! previously HUNG because `serve_session_rmcp` ran the rmcp async serve on a
//! `new_current_thread()` tokio runtime, where `BlockingBridge::poll_read`'s
//! blocking `read()` syscall freezes the single executor thread while it waits
//! for the NEXT client line. `initialize` (handled inline before the serve
//! loop) completed, and so did any request whose bytes were already buffered,
//! but a `tools/call` that arrives AFTER a pause never got its spawned handler
//! polled — the client waited forever (Kiro "Elapsed 2h", Zed "request
//! timeout").
//!
//! This drives the SHIPPED daemon+session path (rmcp is the sole transport):
//! connect a raw client, send the proxy's exact byte sequence
//! (client-hello → initialize → initialized → tools/call) with a real pause
//! before the tools/call so the bridge's read blocks, then assert the daemon
//! answers the tools/call within a bounded timeout.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_daemon::{DaemonOptions, StartOrAttach, start_or_attach};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};

fn connect(socket_path: &Path) -> Stream {
    let name = socket_path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .expect("fs name");
    Stream::connect(name).expect("connect to daemon socket")
}

/// Read newline-framed lines from the stream until one is a JSON-RPC reply for
/// `want_id`, or the deadline elapses. Returns the parsed reply, or `None` on
/// timeout / EOF. Uses a per-read socket timeout so a hung server surfaces as a
/// bounded `None` instead of blocking the test forever.
fn read_reply_for(
    stream: &mut Stream,
    want_id: i64,
    overall_deadline: Instant,
) -> Option<serde_json::Value> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if Instant::now() >= overall_deadline {
            return None;
        }
        // Bound each blocking read so we can re-check the deadline.
        let _ = stream.set_recv_timeout(Some(Duration::from_millis(250)));
        match stream.read(&mut byte) {
            Ok(0) => return None, // EOF
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
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Poll tick: loop to re-check the deadline.
                continue;
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn real_client_tools_call_through_daemon_session_returns_result() {
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };

    // A REAL indexed project so `codegraph_search` actually resolves + runs a
    // tool (mirrors the mcp-crate `real_indexed_project` helper): copy the
    // golden mini db into the temp project's `.codegraph/`.
    let project = temp_indexed_project("rmcp-toolscall");

    // In-process daemon must replicate the binary's `init_config` startup
    // (main.rs) or tool execution panics on the uninitialized global config.
    let _ = codegraph_core::config::init_config(None, &project);

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

    let mut stream = connect(&socket_path);

    // Proxy byte sequence: client-hello, then initialize.
    let client_hello = r#"{"hostPid":424242}"#;
    writeln!(stream, "{client_hello}").unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"regress","version":"0"}}}"#;
    writeln!(stream, "{init}").unwrap();
    stream.flush().unwrap();

    let init_deadline = Instant::now() + Duration::from_secs(10);
    let init_reply = read_reply_for(&mut stream, 1, init_deadline)
        .expect("daemon session must answer initialize");
    assert_eq!(
        init_reply["result"]["serverInfo"]["name"],
        serde_json::json!("codegraph")
    );

    // initialized notification, then a REAL pause so the daemon's transport
    // read is parked waiting for the next line — the state under which the
    // blocking-poll_read bridge deadlocked rmcp's serve-loop select!.
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    writeln!(stream, "{initialized}").unwrap();
    stream.flush().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // tools/call — the request that previously hung.
    let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"codegraph_search","arguments":{"query":"add"}}}"#;
    writeln!(stream, "{call}").unwrap();
    stream.flush().unwrap();

    let call_deadline = Instant::now() + Duration::from_secs(15);
    let call_reply = read_reply_for(&mut stream, 3, call_deadline).expect(
        "daemon session MUST answer tools/call within 15s — a None here is the \
         current-thread-runtime starvation hang (Kiro/Zed timeout)",
    );
    assert!(
        call_reply.get("result").is_some(),
        "tools/call must return a result, got: {call_reply}"
    );

    drop(stream);
    handle.stop().expect("daemon stops");
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };
    let _ = fs::remove_dir_all(&project);
}

static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
