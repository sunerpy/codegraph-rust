use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

pub fn current_ppid() -> u32 {
    parse_ppid_from_proc().unwrap_or(0)
}

pub fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn parse_ppid_from_proc() -> Option<u32> {
    let stat = fs::read_to_string(PathBuf::from("/proc/self/stat")).ok()?;
    let after_name = stat.rsplit_once(") ")?.1;
    let mut fields = after_name.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
