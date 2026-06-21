use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{codegraph_dir, daemon_pid_path, daemon_socket_path};
use crate::process::is_process_alive;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DaemonLockInfo {
    pub pid: u32,
    pub version: String,
    pub socket_path: PathBuf,
    pub started_at: u128,
}

#[derive(Debug)]
pub enum AcquireResult {
    Acquired {
        pid_path: PathBuf,
        info: DaemonLockInfo,
    },
    Taken {
        pid_path: PathBuf,
        existing: Option<DaemonLockInfo>,
    },
}

pub fn encode_lock_info(info: &DaemonLockInfo) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(info)?))
}

pub fn decode_lock_info(raw: &str) -> Option<DaemonLockInfo> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(info) = serde_json::from_str::<DaemonLockInfo>(trimmed) {
        return Some(info);
    }
    trimmed
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .map(|pid| DaemonLockInfo {
            pid,
            version: "unknown".to_string(),
            socket_path: PathBuf::new(),
            started_at: 0,
        })
}

pub fn try_acquire_daemon_lock(project_root: &Path) -> Result<AcquireResult> {
    let pid_path = daemon_pid_path(project_root);
    fs::create_dir_all(codegraph_dir(project_root))
        .with_context(|| format!("creating {}", codegraph_dir(project_root).display()))?;

    let info = DaemonLockInfo {
        pid: process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        socket_path: daemon_socket_path(project_root),
        started_at: now_millis(),
    };

    // Port of upstream mcp/daemon.ts:393-412: write a complete
    // private temp pidfile and hard-link it into place so readers never observe
    // an empty or partial lock record during concurrent daemon startup.
    let tmp = pid_path.with_extension(format!("pid.{}.tmp", process::id()));
    let mut acquired = false;
    fs::write(&tmp, encode_lock_info(&info)?)
        .with_context(|| format!("writing temp daemon lock {}", tmp.display()))?;
    match fs::hard_link(&tmp, &pid_path) {
        Ok(()) => acquired = true,
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err).with_context(|| format!("linking {}", pid_path.display())),
    }
    let _ = fs::remove_file(&tmp);

    if acquired {
        return Ok(AcquireResult::Acquired { pid_path, info });
    }

    let existing = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|raw| decode_lock_info(&raw));
    Ok(AcquireResult::Taken { pid_path, existing })
}

pub fn clear_stale_daemon_lock(pid_path: &Path, expected_dead_pid: Option<u32>) -> bool {
    // Port of upstream mcp/daemon.ts:453-481: compare-and-delete the
    // pidfile only after re-reading it, and never remove a lock held by a live pid.
    let raw = match fs::read_to_string(pid_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if let Some(info) = decode_lock_info(&raw) {
        if expected_dead_pid.is_some_and(|pid| pid != info.pid) {
            return false;
        }
        if info.pid > 0 && is_process_alive(info.pid) {
            return false;
        }
    }
    fs::remove_file(pid_path).is_ok()
}

pub fn unlock_project(project_root: &Path) -> bool {
    let pid_path = daemon_pid_path(project_root);
    clear_stale_daemon_lock(&pid_path, None)
}

pub(crate) fn cleanup_owned_lock(pid_path: &Path, pid: u32) {
    let owned = fs::read_to_string(pid_path)
        .ok()
        .and_then(|raw| decode_lock_info(&raw))
        .is_some_and(|info| info.pid == pid);
    if owned {
        let _ = fs::remove_file(pid_path);
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_plain_pid_decodes() {
        let info = decode_lock_info("1234\n").expect("pid decodes");
        assert_eq!(info.pid, 1234);
        assert_eq!(info.version, "unknown");
    }
}
