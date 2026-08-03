//! Batch M item 20 — the daemon side of the `uninit --force` shutdown control
//! channel.
//!
//! The CLI acceptance tests in `codegraph-rs` drive whole processes. These tests
//! pin the daemon-local guarantees that acceptance cannot observe from outside:
//!
//! 1. an authorized control frame ACTIVELY closes a long-lived data session (the
//!    client's socket reaches EOF) and only then acknowledges the drain;
//! 2. an UNAUTHORIZED frame (foreign project identity) is answered
//!    `drained: false`, mutates nothing, and leaves the daemon serving;
//! 3. an outstanding session at an exhausted drain budget is answered
//!    `drained: false` on the wire — the daemon never reports success it did not
//!    achieve;
//! 4. the caller maps an incomplete-drain reply to
//!    [`ShutdownOutcome::Unresponsive`] and never signals the recorded pid.
//!
//! (3) is deterministic, not timing-dependent: with a zero drain budget the
//! accept loop checks the outstanding counts in the same poll in which it signals
//! the sessions to close, before any session task can be polled again, so the
//! session is provably still counted. (4) is driven against a stub listener that
//! replies `drained: false`.
//!
//! Unix-gated: the stub rendezvous is bound through `GenericFilePath`. The
//! production code paths under test are platform-independent (the session close
//! signal is a `tokio::sync::watch`, not a raw fd), but this host cannot execute
//! the Windows named-pipe arm, so no Windows runtime claim is made here.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codegraph_core::IndexPaths;
use codegraph_daemon::{
    ControlAck, ControlFrame, DaemonHandle, DaemonLockInfo, DaemonOptions, ShutdownOutcome,
    StartOrAttach, encode_lock_info, is_process_alive, request_daemon_shutdown, start_or_attach,
};
use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Stream, ToFsName};

const WAIT: Duration = Duration::from_secs(10);

fn temp_project(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cg-daemon-control-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("create project");
    path.canonicalize().expect("canonicalize project")
}

fn paths_of(project: &Path) -> IndexPaths {
    IndexPaths::resolve(project, None).expect("resolve index paths")
}

fn options(drain_budget: Duration) -> DaemonOptions {
    DaemonOptions {
        run_mcp: true,
        watch: false,
        watchdog_interval: Duration::from_millis(10),
        drain_budget,
        ..DaemonOptions::default()
    }
}

fn start(project: &Path, drain_budget: Duration) -> DaemonHandle {
    match start_or_attach(project, options(drain_budget)).expect("daemon starts") {
        StartOrAttach::Started(handle) => handle,
        StartOrAttach::Attached(_) => panic!("a fresh project must not attach"),
    }
}

fn connect(socket_path: &Path) -> Stream {
    let name = socket_path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .expect("fs name");
    Stream::connect(name).expect("connect to the daemon rendezvous")
}

/// Connect a data client, consume the daemon hello, and leave the connection
/// open and SILENT — the shape that would keep the session count nonzero forever
/// if shutdown merely waited instead of actively closing sessions.
fn connect_silent_data_client(socket_path: &Path) -> Stream {
    let stream = connect(socket_path);
    stream
        .set_recv_timeout(Some(WAIT))
        .expect("bound the client read");
    let mut reader = BufReader::new(&stream);
    let mut hello = String::new();
    reader.read_line(&mut hello).expect("read the daemon hello");
    assert!(
        hello.contains("\"protocol\":1"),
        "the daemon hello must arrive first: {hello}"
    );
    stream
}

fn send_control_frame(socket_path: &Path, frame: &ControlFrame) -> ControlAck {
    let stream = connect(socket_path);
    stream
        .set_recv_timeout(Some(WAIT))
        .expect("bound the control read");
    stream
        .set_send_timeout(Some(WAIT))
        .expect("bound the control write");
    let mut reader = BufReader::new(&stream);
    let mut hello = String::new();
    reader.read_line(&mut hello).expect("read the daemon hello");
    (&stream)
        .write_all(format!("{}\n", serde_json::to_string(frame).expect("frame json")).as_bytes())
        .expect("send the control frame");
    (&stream).flush().expect("flush the control frame");
    let mut ack = String::new();
    reader.read_line(&mut ack).expect("read the control reply");
    serde_json::from_str(ack.trim()).expect("decode the control reply")
}

fn wait_finished(handle: &DaemonHandle, label: &str) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{label} did not finish within {WAIT:?}");
}

#[test]
fn authorized_shutdown_closes_a_long_lived_session_before_acknowledging() {
    let project = temp_project("closes-session");
    let paths = paths_of(&project);
    let handle = start(&project, Duration::from_secs(10));
    let socket = handle.socket_path().to_path_buf();

    let mut silent = connect_silent_data_client(&socket);
    let ack = send_control_frame(&socket, &ControlFrame::shutdown(paths.project_identity()));
    assert!(
        ack.accepted(),
        "an authorized frame must be acknowledged after a completed drain: {ack:?}"
    );

    // The long-lived session was closed by the daemon, not by the client: its
    // socket reads EOF. Reverting the active close makes this read time out.
    let mut rest = Vec::new();
    silent
        .read_to_end(&mut rest)
        .expect("the closed session must EOF, not time out");
    assert!(
        rest.is_empty(),
        "no further bytes are served after shutdown: {rest:?}"
    );

    // The ACK follows rendezvous removal, so both are already gone.
    assert!(!paths.daemon_pid().exists(), "the owner record is removed");
    assert!(!socket.exists(), "the bound socket is removed");
    wait_finished(&handle, "the drained daemon");
    handle.wait().expect("the accept loop joins cleanly");
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_unauthorized_frame_is_refused_without_shutting_the_daemon_down() {
    let project = temp_project("refuses-foreign");
    let paths = paths_of(&project);
    let handle = start(&project, Duration::from_secs(10));
    let socket = handle.socket_path().to_path_buf();

    for frame in [
        ControlFrame::shutdown("0000000000000000000000000000000000000000000000000000000000000000"),
        ControlFrame {
            codegraph_control: codegraph_daemon::CONTROL_PROTOCOL + 1,
            action: "shutdown".to_string(),
            project_identity: paths.project_identity().to_string(),
        },
        ControlFrame {
            codegraph_control: codegraph_daemon::CONTROL_PROTOCOL,
            action: "reindex".to_string(),
            project_identity: paths.project_identity().to_string(),
        },
    ] {
        let ack = send_control_frame(&socket, &frame);
        assert!(
            !ack.accepted(),
            "an unauthorized frame must never be acknowledged: {frame:?} -> {ack:?}"
        );
        assert!(
            paths.daemon_pid().exists() && socket.exists(),
            "a refused frame mutates no rendezvous artifact"
        );
        assert!(!handle.is_finished(), "a refused frame never stops serving");
    }

    // Still serving: an authorized frame afterwards is what actually drains it.
    let ack = send_control_frame(&socket, &ControlFrame::shutdown(paths.project_identity()));
    assert!(ack.accepted());
    wait_finished(&handle, "the drained daemon");
    handle.wait().expect("the accept loop joins cleanly");
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_exhausted_drain_budget_replies_incomplete_instead_of_success() {
    let project = temp_project("budget-exhausted");
    let paths = paths_of(&project);
    // A zero budget cannot be met while any session is still outstanding.
    let handle = start(&project, Duration::ZERO);
    let socket = handle.socket_path().to_path_buf();

    let _silent = connect_silent_data_client(&socket);
    let ack = send_control_frame(&socket, &ControlFrame::shutdown(paths.project_identity()));
    assert!(
        !ack.drained,
        "an unmet drain budget must reply with an INCOMPLETE drain: {ack:?}"
    );
    assert!(
        !ack.accepted(),
        "an incomplete drain must never be accepted as success"
    );

    // The daemon still tears down only what it owns, so a repeated `uninit --force`
    // can continue; nothing else was touched.
    wait_finished(&handle, "the daemon after an incomplete drain");
    handle.wait().expect("the accept loop joins cleanly");
    assert!(!paths.daemon_pid().exists());
    assert!(
        paths.current_root().is_dir(),
        "the namespace itself survives"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_incomplete_drain_reply_is_unresponsive_and_never_signals_the_owner() {
    let project = temp_project("incomplete-drain");
    let paths = paths_of(&project);
    std::fs::create_dir_all(paths.current_root()).expect("create the v2 rendezvous dir");
    let socket = paths.current_root().join("daemon.sock");

    // A stub rendezvous that answers every control frame with an INCOMPLETE drain,
    // recorded as owned by THIS live process.
    let listener = ListenerOptions::new()
        .name(
            socket
                .as_os_str()
                .to_fs_name::<GenericFilePath>()
                .expect("fs name"),
        )
        .create_sync()
        .expect("bind the stub rendezvous");
    let stub = std::thread::spawn(move || {
        let Some(Ok(stream)) = listener.incoming().next() else {
            return;
        };
        let _ = stream.set_send_timeout(Some(WAIT));
        let _ = stream.set_recv_timeout(Some(WAIT));
        let mut reader = BufReader::new(&stream);
        let _ = (&stream).write_all(b"{\"codegraph\":\"stub\",\"protocol\":1}\n");
        let _ = (&stream).flush();
        let mut frame = String::new();
        let _ = reader.read_line(&mut frame);
        let reply = serde_json::to_string(&ControlAck::for_drain(false)).expect("reply json");
        let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
        let _ = (&stream).flush();
    });

    let owner = DaemonLockInfo {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        socket_path: socket.clone(),
        started_at: 1,
    };
    std::fs::write(
        paths.daemon_pid(),
        encode_lock_info(&owner).expect("encode the owner record"),
    )
    .expect("write the owner record");

    let outcome = request_daemon_shutdown(&project, paths.project_identity())
        .expect("an incomplete drain is data, not an error");
    match outcome {
        ShutdownOutcome::Unresponsive { pid, detail } => {
            assert_eq!(pid, std::process::id());
            assert!(
                detail.contains("incomplete drain"),
                "the refusal must name the incomplete drain: {detail}"
            );
        }
        other => panic!("an incomplete drain must be unresponsive, got {other:?}"),
    }
    assert!(
        is_process_alive(std::process::id()),
        "the recorded owner is never signalled"
    );
    assert!(
        paths.daemon_pid().exists(),
        "a fail-closed exchange removes no rendezvous artifact"
    );

    let _ = stub.join();
    let _ = std::fs::remove_dir_all(&project);
}
