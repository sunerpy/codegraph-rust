use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::paths::daemon_log_path;
use crate::CODEGRAPH_DAEMON_INTERNAL;

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Spawn a detached background daemon by re-invoking `exe serve --mcp --path
/// <root>` with `CODEGRAPH_DAEMON_INTERNAL=1`, redirecting stdout+stderr to an
/// appended `.codegraph/daemon.log`, detaching it from this process group, then
/// dropping the child handle (the Rust equivalent of Node's `unref`). The
/// executable path is a parameter so the daemon crate stays testable; the CLI
/// passes `std::env::current_exe()?`.
pub fn spawn_detached_daemon(exe: &Path, root: &Path) -> Result<()> {
    let mut command = Command::new(exe);
    command
        .arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        .env(CODEGRAPH_DAEMON_INTERNAL, "1")
        .stdin(Stdio::null())
        .stdout(log_target(root))
        .stderr(log_target(root));

    detach(&mut command);

    let child = command
        .spawn()
        .with_context(|| format!("spawning detached daemon via {}", exe.display()))?;
    drop(child);
    Ok(())
}

fn log_target(root: &Path) -> Stdio {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path(root))
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY / async-signal-safety contract: `pre_exec` runs in the forked child
    // AFTER fork() and BEFORE exec(). In that window only async-signal-safe work
    // is permitted (no heap allocation, no locks, no Rust std I/O) because the
    // child shares the parent's address space until exec and the runtime is in an
    // indeterminate state. The closure below calls EXACTLY ONE thing —
    // `setsid()`, a bare async-signal-safe syscall (rustix issues it directly,
    // no allocation) — and returns its result. Nothing else runs here.
    //
    // Why setsid (the zombie fix): `process_group(0)` only put the daemon in a
    // new process GROUP; it stayed a DIRECT child of the spawning `serve --mcp`
    // proxy. When the daemon exited while that proxy lived on (and never wait()ed
    // it), it became a `<defunct>` zombie holding a process-table slot. `setsid()`
    // makes the daemon a SESSION LEADER in a brand-new session, so once the
    // spawning proxy exits the daemon is reparented to init (PID 1) and an exiting
    // daemon is reaped by init — no zombie. setsid already creates a new process
    // group, so the previous `process_group(0)` call is now redundant and removed.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()?;
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}
