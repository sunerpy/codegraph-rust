use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::CODEGRAPH_DAEMON_INTERNAL;
use crate::paths::daemon_log_path;

/// Env var the detached HTTP child reads to know it IS the background server and
/// must run the foreground serve path (NOT re-detach). Set on the spawned child
/// by [`spawn_detached_http`]; the CLI checks it before deciding to detach.
pub const CODEGRAPH_HTTP_DETACH_INTERNAL: &str = "CODEGRAPH_HTTP_DETACH_INTERNAL";

/// Env var the daemon child reads to disable its live file watcher. Kept in
/// sync with `codegraph_watch::CODEGRAPH_NO_WATCH`; duplicated here so the
/// daemon crate does not need to depend on codegraph-watch just for a string.
const CODEGRAPH_NO_WATCH: &str = "CODEGRAPH_NO_WATCH";

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Spawn a detached background daemon by re-invoking `exe serve --mcp --path
/// <root>` with `CODEGRAPH_DAEMON_INTERNAL=1`, redirecting stdout+stderr to an
/// appended `.codegraph/daemon.log`, detaching it from this process group, and
/// keeping a tiny reaper thread to wait on the child when it exits. The
/// executable path is a parameter so the daemon crate stays testable; the CLI
/// passes `std::env::current_exe()?`.
///
/// `no_watch` forwards the `--no-watch` intent to the detached child EXPLICITLY
/// via `Command.env` instead of mutating this process's global environment.
/// When `true`, the child sees `CODEGRAPH_NO_WATCH=1` and disables its live
/// file watcher, exactly as if the flag had been inherited — but without any
/// global-env mutation in the parent.
pub fn spawn_detached_daemon(exe: &Path, root: &Path, no_watch: bool) -> Result<()> {
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
    if no_watch {
        command.env(CODEGRAPH_NO_WATCH, "1");
    }

    detach(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawning detached daemon via {}", exe.display()))?;
    let _reaper = std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn log_target(root: &Path) -> Stdio {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path(root))
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

/// Spawn a detached background HTTP MCP server by re-invoking `exe serve --http
/// --http-addr <addr> [--path <project>]` with `CODEGRAPH_HTTP_DETACH_INTERNAL=1`
/// so the child runs the foreground serve path and does NOT re-detach. Stdout
/// and stderr are appended to `log_file`; the child is detached from this
/// process group (shared [`detach`] primitive) and a reaper thread waits on it.
/// Returns the child pid so the parent can write the registry entry.
pub fn spawn_detached_http(
    exe: &Path,
    addr: &str,
    project: Option<&Path>,
    log_file: &Path,
) -> Result<u32> {
    let mut command = Command::new(exe);
    command
        .arg("serve")
        .arg("--http")
        .arg("--http-addr")
        .arg(addr)
        .env(CODEGRAPH_HTTP_DETACH_INTERNAL, "1")
        .stdin(Stdio::null())
        .stdout(open_log(log_file))
        .stderr(open_log(log_file));
    if let Some(project) = project {
        command.arg("--path").arg(project);
    }

    detach(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawning detached HTTP server via {}", exe.display()))?;
    let pid = child.id();
    let _reaper = std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn open_log(path: &Path) -> Stdio {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
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
    // Why setsid: `process_group(0)` only put the daemon in a new process GROUP;
    // it stayed in the proxy's session. `setsid()` makes the daemon a SESSION
    // LEADER in a brand-new session, so terminal/session signals from the host do
    // not reach the shared daemon. Reaping is handled by `spawn_detached_daemon`'s
    // child-wait thread; setsid alone does not make a live parent stop owning the
    // child process table entry.
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

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::spawn_detached_daemon;

    fn temp_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codegraph-spawn-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn zombie_children_of_current_process() -> usize {
        let parent_pid = std::process::id();
        let Ok(entries) = fs::read_dir("/proc") else {
            return 0;
        };

        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
            .filter_map(|pid| fs::read_to_string(format!("/proc/{pid}/stat")).ok())
            .filter(|stat| {
                let Some((_, rest)) = stat.rsplit_once(')') else {
                    return false;
                };
                let mut fields = rest.split_whitespace();
                let state = fields.next();
                let ppid = fields.next().and_then(|raw| raw.parse::<u32>().ok());
                state == Some("Z") && ppid == Some(parent_pid)
            })
            .count()
    }

    fn eventually_no_zombie_children(timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if zombie_children_of_current_process() == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        zombie_children_of_current_process() == 0
    }

    #[test]
    fn spawn_detached_daemon_reaps_exited_child() {
        let exe = Path::new("/bin/true");
        assert!(
            exe.exists(),
            "/bin/true is required for this Unix lifecycle test"
        );
        let root = temp_root("reap-exited-child");

        spawn_detached_daemon(exe, &root, false).expect("spawn short-lived daemon command");

        assert!(
            eventually_no_zombie_children(Duration::from_secs(1)),
            "spawn_detached_daemon must reap an exited child instead of leaving a zombie"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn spawn_detached_http_returns_pid_and_reaps_child() {
        let exe = Path::new("/bin/true");
        assert!(exe.exists(), "/bin/true is required for this test");
        let root = temp_root("http-reap");
        let log = root.join("http.log");

        let pid = super::spawn_detached_http(exe, "127.0.0.1:0", Some(&root), &log)
            .expect("spawn short-lived http command");
        assert!(pid > 0, "spawn_detached_http must return the child pid");

        assert!(
            eventually_no_zombie_children(Duration::from_secs(1)),
            "spawn_detached_http must reap an exited child instead of leaving a zombie"
        );

        let _ = fs::remove_dir_all(root);
    }
}
