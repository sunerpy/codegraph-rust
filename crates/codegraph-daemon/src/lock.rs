use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{codegraph_dir, daemon_pid_path, daemon_socket_path};
use crate::process::is_process_alive;

const EMPTY_RETRY_DELAY: Duration = Duration::from_millis(20);

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

    // Port of upstream mcp/daemon.ts:393-412: write a complete private temp
    // pidfile, then atomically claim the final path by renaming the temp over a
    // freshly created (create_new) placeholder. Renaming the fully-written temp
    // means a concurrent reader never observes an empty or partial lock record.
    let payload = encode_lock_info(&info)?;
    let tmp = pid_path.with_extension(format!("pid.{}.tmp", process::id()));
    fs::write(&tmp, &payload)
        .with_context(|| format!("writing temp daemon lock {}", tmp.display()))?;

    let acquired = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pid_path)
    {
        Ok(_placeholder) => {
            fs::rename(&tmp, &pid_path)
                .with_context(|| format!("publishing daemon lock {}", pid_path.display()))?;
            true
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            false
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err).with_context(|| format!("claiming {}", pid_path.display()));
        }
    };

    if acquired {
        return Ok(AcquireResult::Acquired { pid_path, info });
    }

    let existing = read_lock_info_tolerant(&pid_path);
    Ok(AcquireResult::Taken { pid_path, existing })
}

pub fn clear_stale_daemon_lock(pid_path: &Path, expected_dead_pid: Option<u32>) -> bool {
    // Port of upstream mcp/daemon.ts:453-481: compare-and-delete the
    // pidfile only after re-reading it, and never remove a lock held by a live pid.
    let raw = match read_pidfile_tolerant(pid_path) {
        ReadOutcome::Missing => return true,
        ReadOutcome::Unreadable => return false,
        // An empty pidfile is an in-flight publish (create_new placeholder before
        // the rename lands); treat as live, never delete on empty.
        ReadOutcome::Empty => return false,
        ReadOutcome::Content(raw) => raw,
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
    let owned = read_lock_info_tolerant(pid_path).is_some_and(|info| info.pid == pid);
    if owned {
        let _ = fs::remove_file(pid_path);
    }
}

enum ReadOutcome {
    Missing,
    Unreadable,
    Empty,
    Content(String),
}

fn read_pidfile_once(pid_path: &Path) -> ReadOutcome {
    match fs::read_to_string(pid_path) {
        Ok(raw) if raw.trim().is_empty() => ReadOutcome::Empty,
        Ok(raw) => ReadOutcome::Content(raw),
        Err(err) if err.kind() == ErrorKind::NotFound => ReadOutcome::Missing,
        Err(_) => ReadOutcome::Unreadable,
    }
}

fn read_pidfile_tolerant(pid_path: &Path) -> ReadOutcome {
    match read_pidfile_once(pid_path) {
        // Retry once after a short sleep: an empty pidfile is an in-flight
        // create_new placeholder whose rename has not landed yet.
        ReadOutcome::Empty => {
            thread::sleep(EMPTY_RETRY_DELAY);
            read_pidfile_once(pid_path)
        }
        other => other,
    }
}

fn read_lock_info_tolerant(pid_path: &Path) -> Option<DaemonLockInfo> {
    match read_pidfile_tolerant(pid_path) {
        ReadOutcome::Content(raw) => decode_lock_info(&raw),
        _ => None,
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
