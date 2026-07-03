//! T7 unit coverage for the proxy hello-verify helper.
//!
//! These assertions need NO real daemon: `verify_daemon_hello` is a pure
//! function over the parsed `DaemonHello` JSON, exposed `pub` so this external
//! test crate can drive the mismatch branch directly.

#![cfg(unix)]

use codegraph_daemon::{ProxyOutcome, verify_daemon_hello};
use serde_json::json;

#[test]
fn matching_hello_proceeds() {
    let hello = json!({
        "codegraph": env!("CARGO_PKG_VERSION"),
        "pid": 1234,
        "socketPath": "/tmp/x.sock",
        "protocol": 1,
    });
    assert_eq!(
        verify_daemon_hello(&hello),
        None,
        "same version+protocol proceeds"
    );
}

#[test]
fn wrong_version_is_mismatch() {
    let hello = json!({
        "codegraph": "0.0.0-not-this-build",
        "pid": 1234,
        "socketPath": "/tmp/x.sock",
        "protocol": 1,
    });
    assert_eq!(
        verify_daemon_hello(&hello),
        Some(ProxyOutcome::VersionMismatch),
        "a wrong codegraph version must force VersionMismatch (direct fallback)"
    );
}

#[test]
fn wrong_protocol_is_mismatch() {
    let hello = json!({
        "codegraph": env!("CARGO_PKG_VERSION"),
        "pid": 1234,
        "socketPath": "/tmp/x.sock",
        "protocol": 2,
    });
    assert_eq!(
        verify_daemon_hello(&hello),
        Some(ProxyOutcome::VersionMismatch),
        "a wrong protocol must force VersionMismatch"
    );
}

#[test]
fn missing_fields_are_mismatch() {
    let hello = json!({ "pid": 1234 });
    assert_eq!(
        verify_daemon_hello(&hello),
        Some(ProxyOutcome::VersionMismatch),
        "a hello missing version/protocol must not be treated as a match"
    );
}
