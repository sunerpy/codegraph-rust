//! Long-lived per-project writer ownership for MCP watcher/catch-up services.
//!
//! The permanent `index.lock` serializes one mutation at a time. It does not stop
//! two long-lived direct MCP processes from alternating watcher syncs forever.
//! `writer.pid` closes that lifecycle gap: one shared daemon, or one explicit
//! direct process, holds an OS-level exclusive lock for the lifetime of its
//! background writes. The JSON stored in the same file is diagnostic only; the
//! kernel lock, not PID liveness, is authority.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use codegraph_core::IndexPaths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WriterLockInfo {
    pub pid: u32,
    pub mode: String,
    pub started_at: u128,
}

#[derive(Debug)]
pub enum WriterAcquireResult {
    Acquired(WriterLockGuard),
    Taken {
        pid_path: PathBuf,
        existing: Option<WriterLockInfo>,
    },
}

/// RAII ownership of the kernel-locked `writer.pid` handle.
///
/// The path is deliberately persistent. Drop clears the diagnostic bytes while
/// still holding the kernel lock, then unlocks and closes the handle. Never
/// unlinking the authority path avoids the classic unlock/unlink/recreate ABA
/// race and lets process death release ownership automatically on every OS.
#[derive(Debug)]
pub struct WriterLockGuard {
    file: File,
    pid_path: PathBuf,
    info: WriterLockInfo,
}

impl WriterLockGuard {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.pid_path
    }

    #[must_use]
    pub fn info(&self) -> &WriterLockInfo {
        &self.info
    }
}

impl Drop for WriterLockGuard {
    fn drop(&mut self) {
        // Clear diagnostics before releasing authority. Failure is best-effort:
        // closing the file still releases the kernel lock, and the next owner
        // overwrites the whole record after acquiring it.
        let _ = clear_file(&mut self.file);
        let _ = self.file.unlock();
    }
}

pub fn writer_pid_path(project_root: &Path) -> Result<PathBuf> {
    let paths = IndexPaths::resolve(project_root, std::env::var("CODEGRAPH_DIR").ok().as_deref())?;
    Ok(paths.current_root().join("writer.pid"))
}

pub fn decode_writer_lock_info(raw: &str) -> Option<WriterLockInfo> {
    let info = serde_json::from_str::<WriterLockInfo>(raw.trim()).ok()?;
    (info.pid > 0 && !info.mode.trim().is_empty()).then_some(info)
}

/// Return diagnostics only when another process currently holds the kernel lock.
/// An idle persistent `writer.pid` therefore reads as `None` even if stale bytes
/// survived an unclean shutdown.
pub fn read_writer_lock(project_root: &Path) -> Option<WriterLockInfo> {
    let path = writer_pid_path(project_root).ok()?;
    let mut file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            None
        }
        Err(std::fs::TryLockError::WouldBlock) => read_info_from(&mut file),
        Err(std::fs::TryLockError::Error(_)) => None,
    }
}

/// Try once to claim long-lived writer ownership. The current index root must
/// already exist; this API never initializes a project or creates `.codegraph`.
pub fn try_acquire_writer_lock(
    project_root: &Path,
    mode: impl Into<String>,
) -> Result<WriterAcquireResult> {
    let pid_path = writer_pid_path(project_root)?;
    let parent = pid_path
        .parent()
        .expect("writer.pid is always inside the resolved index root");
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("opening existing index root {}", parent.display()))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        bail!(
            "writer lock parent is not a physical directory: {}",
            parent.display()
        );
    }

    // Reject a pre-existing alias/non-file before opening it. IndexPaths already
    // rejects aliased parent components; this closes the final-entry case.
    if let Ok(metadata) = fs::symlink_metadata(&pid_path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        bail!(
            "writer lock path is not a regular physical file: {}",
            pid_path.display()
        );
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&pid_path)
        .with_context(|| format!("opening {}", pid_path.display()))?;
    if !file
        .metadata()
        .with_context(|| format!("inspecting {}", pid_path.display()))?
        .is_file()
    {
        bail!(
            "writer lock handle is not a regular file: {}",
            pid_path.display()
        );
    }

    match file.try_lock() {
        Ok(()) => {
            if !codegraph_store::path_still_names_file(&pid_path, &file)
                .with_context(|| format!("corroborating {}", pid_path.display()))?
            {
                let _ = file.unlock();
                bail!(
                    "writer lock path changed during acquisition: {}",
                    pid_path.display()
                );
            }
            let info = WriterLockInfo {
                pid: process::id(),
                mode: mode.into(),
                started_at: now_millis(),
            };
            write_info(&mut file, &info)
                .with_context(|| format!("publishing {}", pid_path.display()))?;
            Ok(WriterAcquireResult::Acquired(WriterLockGuard {
                file,
                pid_path,
                info,
            }))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(WriterAcquireResult::Taken {
            existing: read_info_from(&mut file),
            pid_path,
        }),
        Err(std::fs::TryLockError::Error(source)) => {
            Err(source).with_context(|| format!("locking {}", pid_path.display()))
        }
    }
}

#[must_use]
pub fn writer_lock_held_message(existing: Option<&WriterLockInfo>, pid_path: &Path) -> String {
    let holder = existing.map_or_else(
        || "another process".to_string(),
        |info| format!("PID {} ({} mode)", info.pid, info.mode),
    );
    format!(
        "CodeGraph writer lock held by {holder}. Only one live MCP writer may own auto-sync for a project. Stop the other server, or unset CODEGRAPH_NO_DAEMON so additional clients proxy to the shared daemon. The kernel lock at {} releases automatically when its owner exits.",
        pid_path.display()
    )
}

/// Clear stale diagnostic bytes only when no live writer holds the kernel lock.
/// The persistent authority file itself is never removed.
pub fn clear_stale_writer_lock(project_root: &Path) -> bool {
    let Ok(path) = writer_pid_path(project_root) else {
        return false;
    };
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    if file.try_lock().is_err() {
        return false;
    }
    let had_bytes = file.metadata().is_ok_and(|metadata| metadata.len() > 0);
    let cleared = clear_file(&mut file).is_ok();
    let _ = file.unlock();
    had_bytes && cleared
}

fn read_info_from(file: &mut File) -> Option<WriterLockInfo> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    decode_writer_lock_info(&raw)
}

fn write_info(file: &mut File, info: &WriterLockInfo) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer_pretty(&mut *file, info).map_err(std::io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_data()
}

fn clear_file(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.sync_data()
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn project(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codegraph-writer-{label}-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join(".codegraph")).unwrap();
        path
    }

    #[test]
    fn acquire_publish_and_drop_clear_the_active_record() {
        let root = project("lifecycle");
        let guard = match try_acquire_writer_lock(&root, "direct").unwrap() {
            WriterAcquireResult::Acquired(guard) => guard,
            other => panic!("unexpected acquisition result: {other:?}"),
        };
        assert_eq!(read_writer_lock(&root).unwrap(), *guard.info());
        assert!(guard.path().is_file());
        let path = guard.path().to_path_buf();
        drop(guard);
        assert!(read_writer_lock(&root).is_none());
        assert!(
            path.is_file(),
            "authority path remains stable between owners"
        );
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_live_owner_is_never_stolen_even_in_the_same_process() {
        let root = project("taken");
        let first = match try_acquire_writer_lock(&root, "daemon").unwrap() {
            WriterAcquireResult::Acquired(guard) => guard,
            other => panic!("unexpected acquisition result: {other:?}"),
        };
        match try_acquire_writer_lock(&root, "direct").unwrap() {
            WriterAcquireResult::Taken { existing, .. } => {
                assert_eq!(existing.unwrap().pid, process::id());
            }
            other => panic!("a second writer must be rejected: {other:?}"),
        }
        drop(first);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_bytes_are_overwritten_after_kernel_ownership_is_free() {
        let root = project("stale");
        let path = writer_pid_path(&root).unwrap();
        fs::write(&path, r#"{"pid":4294967294,"mode":"direct","startedAt":1}"#).unwrap();
        assert!(read_writer_lock(&root).is_none());
        let guard = match try_acquire_writer_lock(&root, "daemon").unwrap() {
            WriterAcquireResult::Acquired(guard) => guard,
            other => panic!("free kernel lock should be acquired: {other:?}"),
        };
        assert_eq!(guard.info().pid, process::id());
        assert_eq!(guard.info().mode, "daemon");
        drop(guard);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unindexed_project_is_never_initialized_by_writer_acquisition() {
        let root = std::env::temp_dir().join(format!(
            "codegraph-writer-unindexed-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(try_acquire_writer_lock(&root, "direct").is_err());
        assert!(!root.join(".codegraph").exists());
        let _ = fs::remove_dir_all(root);
    }
}
