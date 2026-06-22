#[derive(Clone, Debug)]
pub struct SupervisionState {
    pub original_ppid: u32,
    pub current_ppid: u32,
    pub host_pid: Option<u32>,
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
    #[cfg(unix)]
    if state.current_ppid != state.original_ppid {
        return Some(format!(
            "ppid {} -> {}",
            state.original_ppid, state.current_ppid
        ));
    }
    if let Some(host_pid) = state.host_pid {
        if !is_alive(host_pid) {
            return Some(format!("host pid {host_pid} exited"));
        }
    }
    None
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

// Windows has no stable getppid; returning 0 makes the ppid-divergence branch
// inert (it is `#[cfg(unix)]`-gated anyway — see `supervision_lost_reason`).
#[cfg(windows)]
pub fn current_ppid() -> u32 {
    0
}

#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FALSE, STILL_ACTIVE,
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
        };
        assert_eq!(
            supervision_lost_reason(&state, |_| true),
            Some("ppid 10 -> 1".to_string())
        );
    }

    #[test]
    fn supervision_detects_dead_host() {
        let state = SupervisionState {
            original_ppid: 10,
            current_ppid: 10,
            host_pid: Some(20),
        };
        assert_eq!(
            supervision_lost_reason(&state, |pid| pid != 20),
            Some("host pid 20 exited".to_string())
        );
    }
}
