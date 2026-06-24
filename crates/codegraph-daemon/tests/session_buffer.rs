//! T9 buffer-safety regression guard.
//!
//! The hazard this locks: the OLD `read_daemon_hello` built a THROWAWAY
//! `BufReader`, read one line, and dropped it — any JSON-RPC bytes the peer had
//! already sent in the SAME chunk after the hello newline were lost, hanging the
//! session. The fix is `run_session_recv`: one long-lived reader reads the
//! OPTIONAL client-hello, then hands the SAME reader (never a fresh one) to
//! `McpServer::run`, so a hello-then-`initialize` arriving in one buffer still
//! gets the `initialize` answered.
//!
//! `run_session_recv` is `pub` + generic over `BufRead`/`Write`, so this
//! external crate drives it with an in-memory `Cursor` — no CLI, no real daemon.

use std::io::{BufReader, Cursor};

use codegraph_daemon::run_session_recv;

/// A minimal JSON-RPC `initialize` request frame (one line).
fn initialize_frame() -> String {
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_string()
}

/// `initialize` is answered by `McpServer` regardless of index, so the output
/// must contain a JSON-RPC response carrying the request id and a `result`.
fn assert_initialize_answered(out: &[u8]) {
    let text = String::from_utf8(out.to_vec()).expect("writer output is UTF-8");
    let response_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("expected a JSON-RPC response line, got: {text:?}"));
    let value: serde_json::Value =
        serde_json::from_str(response_line).expect("response line is JSON");
    assert_eq!(
        value.get("id").and_then(serde_json::Value::as_i64),
        Some(1),
        "initialize response must echo the request id (frame was not lost): {response_line}"
    );
    assert!(
        value.get("result").is_some(),
        "initialize response must carry a result: {response_line}"
    );
}

/// HELLO + INITIALIZE in ONE buffer: the client-hello line is immediately
/// followed by a full `initialize` frame in the same byte stream. The seam must
/// consume the hello AND still answer the `initialize` (the frame is NOT
/// dropped to a throwaway reader). It must also report the parsed host pid.
#[test]
fn client_hello_then_initialize_in_one_buffer_is_not_lost() {
    let bytes = format!("{}\n{}\n", r#"{"hostPid":4321}"#, initialize_frame());
    let reader = BufReader::new(Cursor::new(bytes.into_bytes()));
    let mut out: Vec<u8> = Vec::new();

    let project = std::env::temp_dir().join("codegraph-session-buffer-hello");
    let pid = run_session_recv(reader, &mut out, project, true).expect("run_session_recv ok");

    assert_eq!(
        pid,
        Some(4321),
        "the client-hello hostPid must be parsed and returned"
    );
    assert_initialize_answered(&out);
}

/// NO client-hello: the buffer is just an `initialize` frame. The non-hello
/// first line must NOT be consumed/dropped — it is the first JSON-RPC frame and
/// must be answered, with no pid reported.
#[test]
fn no_client_hello_first_frame_is_answered() {
    let bytes = format!("{}\n", initialize_frame());
    let reader = BufReader::new(Cursor::new(bytes.into_bytes()));
    let mut out: Vec<u8> = Vec::new();

    let project = std::env::temp_dir().join("codegraph-session-buffer-nohello");
    let pid = run_session_recv(reader, &mut out, project, true).expect("run_session_recv ok");

    assert_eq!(pid, None, "a client that sent no hello reports no pid");
    assert_initialize_answered(&out);
}
