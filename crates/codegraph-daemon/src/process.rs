#[derive(Clone, Debug)]
pub struct SupervisionState {
    pub original_ppid: u32,
    pub current_ppid: u32,
    pub host_pid: Option<u32>,
    /// True when this process is its own session leader (it called `setsid`).
    /// A session leader was DELIBERATELY daemonized and reparents to init on
    /// spawner exit, so ppid divergence is expected, not orphaning — suppress
    /// the ppid branch and rely on `host_pid` liveness. The proxy (NOT a session
    /// leader) sets this `false` and keeps the ppid-divergence signal.
    pub session_leader: bool,
}

pub fn supervision_lost_reason<F>(state: &SupervisionState, is_alive: F) -> Option<String>
where
    F: Fn(u32) -> bool,
{
    // Port of upstream mcp/ppid-watchdog.ts:48-61: POSIX orphaning
    // is detected by ppid divergence; a known host pid is also liveness-polled.
    //
    // Windows has no stable getppid (`current_ppid()` returns 0), so the ppid
    // branch is unix-only: comparing a real `original_ppid` against 0 there
    // would self-kill the daemon on its first watchdog tick (audit BUG #5/#8).
    //
    // Option (b) (zombie-reaping fix): the detached daemon now `setsid`s into a
    // new session and reparents to init (PID 1) when its spawner exits. That
    // makes ppid divergence the NORMAL state for a session leader, so a session
    // leader MUST NOT self-trip on it — doing so would suicide the daemon on its
    // first tick. A genuinely dead host is still caught by the `host_pid`
    // liveness branch below, which the proxy announces via the client-hello.
    #[cfg(unix)]
    if !state.session_leader && state.current_ppid != state.original_ppid {
        return Some(format!(
            "ppid {} -> {}",
            state.original_ppid, state.current_ppid
        ));
    }
    if let Some(host_pid) = state.host_pid
        && !is_alive(host_pid)
    {
        return Some(format!("host pid {host_pid} exited"));
    }
    None
}

/// True when the calling process is its own session leader (`getsid(0) ==
/// getpid()`), i.e. it has run `setsid`. The detached daemon uses this to mark
/// its [`SupervisionState`] so ppid divergence after reparenting to init does
/// not falsely read as orphaning. Always `false` on Windows (no sessions).
#[cfg(unix)]
pub fn is_session_leader() -> bool {
    matches!(
        rustix::process::getsid(None),
        Ok(sid) if sid == rustix::process::getpid()
    )
}

#[cfg(windows)]
pub fn is_session_leader() -> bool {
    false
}

#[cfg(unix)]
pub fn current_ppid() -> u32 {
    rustix::process::getppid().map_or(0, |pid| pid.as_raw_nonzero().get() as u32)
}

#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return false;
    };
    // signal-0 liveness: ESRCH means gone; EPERM (or Ok) means the pid exists.
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

/// Send a graceful termination request to `pid` (SIGTERM on unix). Returns true
/// when the signal was delivered. Used by `codegraph http stop` to stop a
/// background HTTP MCP server by its recorded pid.
#[cfg(unix)]
pub fn terminate_pid(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return false;
    };
    rustix::process::kill_process(pid, rustix::process::Signal::Term).is_ok()
}

// Windows has no stable getppid; returning 0 makes the ppid-divergence branch
// inert (it is `#[cfg(unix)]`-gated anyway — see `supervision_lost_reason`).
#[cfg(windows)]
pub fn current_ppid() -> u32 {
    0
}

#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, FALSE, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }

    // SAFETY: OpenProcess/GetExitCodeProcess/CloseHandle are FFI calls with no
    // Rust-side aliasing; `code` is a stack local we only read after a TRUE
    // return. A null handle means we could not open the process: ACCESS_DENIED
    // proves it exists (treat as alive); any other error means it is gone.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle == 0 {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let got = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        got != FALSE && code == STILL_ACTIVE as u32
    }
}

/// Send a termination request to `pid` (Windows: `TerminateProcess`). Returns
/// true when the request was issued. Windows analog of the unix SIGTERM path
/// used by `codegraph http stop`.
#[cfg(windows)]
pub fn terminate_pid(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess/TerminateProcess/CloseHandle are FFI calls with no
    // Rust-side aliasing. A null handle means the open failed (nothing to kill).
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
        if handle == 0 {
            return false;
        }
        let ok = TerminateProcess(handle, 1);
        CloseHandle(handle);
        ok != FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn supervision_detects_ppid_change() {
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 1,
            host_pid: None,
            session_leader: false,
        };
        assert_eq!(
            supervision_lost_reason(&state, |_| true),
            Some("ppid 10 -> 1".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervision_ignores_ppid_change_for_session_leader() {
        // A deliberately-daemonized session leader reparents to init (ppid -> 1)
        // by design, so the SAME divergence that orphans the proxy must NOT trip
        // the daemon — otherwise it suicides on its first watchdog tick (the
        // zombie-reaping regression this fix prevents).
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 1,
            host_pid: None,
            session_leader: true,
        };
        assert_eq!(supervision_lost_reason(&state, |_| true), None);
    }

    #[test]
    fn supervision_detects_dead_host() {
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 10,
            host_pid: Some(20),
            session_leader: false,
        };
        assert_eq!(
            supervision_lost_reason(&state, |pid| pid != 20),
            Some("host pid 20 exited".to_string())
        );
    }

    #[test]
    fn supervision_session_leader_still_detects_dead_host() {
        // The host_pid liveness path MUST keep firing for a session leader: that
        // is the ONLY signal left after the ppid branch is suppressed, and it is
        // what makes the detached daemon exit when its real host actually dies.
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 1,
            host_pid: Some(20),
            session_leader: true,
        };
        assert_eq!(
            supervision_lost_reason(&state, |pid| pid != 20),
            Some("host pid 20 exited".to_string())
        );
    }

    #[test]
    fn is_process_alive_true_for_self_false_for_zero_and_absent() {
        assert!(is_process_alive(std::process::id()));
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(4_000_000_000));
    }

    #[test]
    fn terminate_pid_rejects_zero_and_absent_pids() {
        assert!(!terminate_pid(0));
        assert!(!terminate_pid(4_000_000_000));
    }

    #[test]
    fn is_session_leader_and_current_ppid_do_not_panic() {
        let _ = is_session_leader();
        let _ = current_ppid();
    }

    #[test]
    fn supervision_returns_none_when_supervisor_intact() {
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 10,
            host_pid: Some(20),
            session_leader: false,
        };
        assert_eq!(supervision_lost_reason(&state, |_| true), None);
    }
}
