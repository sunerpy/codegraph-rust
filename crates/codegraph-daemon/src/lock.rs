use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::paths::{daemon_pid_path, daemon_socket_path, rendezvous_dir};
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
    let pid_path = daemon_pid_path(project_root)?;
    let dir = rendezvous_dir(project_root)?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let info = DaemonLockInfo {
        pid: process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        socket_path: daemon_socket_path(project_root)?,
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

/// Rewrite the lock's recorded `socket_path` to `socket_path` (`f83a1ec`),
/// preserving pid/version/started_at. The daemon calls this after bind-fallback
/// selects a socket other than the one recorded at acquire time, so the client
/// reading the lock attaches to the socket the daemon actually bound.
pub fn rewrite_lock_socket_path(pid_path: &Path, socket_path: &Path) -> Result<()> {
    let raw = fs::read_to_string(pid_path)
        .with_context(|| format!("reading daemon lock {}", pid_path.display()))?;
    let mut info = decode_lock_info(&raw)
        .with_context(|| format!("decoding daemon lock {}", pid_path.display()))?;
    info.socket_path = socket_path.to_path_buf();
    fs::write(pid_path, encode_lock_info(&info)?)
        .with_context(|| format!("rewriting daemon lock {}", pid_path.display()))?;
    Ok(())
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

/// Clear a stale daemon lock for `project_root`. An unresolvable index root is
/// reported as "not cleared" rather than reconstructing a rendezvous path.
pub fn unlock_project(project_root: &Path) -> bool {
    daemon_pid_path(project_root)
        .map(|pid_path| clear_stale_daemon_lock(&pid_path, None))
        .unwrap_or(false)
}

/// The mutation boundaries of [`clear_stale_daemon_socket`], exposed only so a
/// test can drive a competing daemon start at an exact point with no timing
/// assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaleHealCheckpoint {
    /// The stale (dead/absent-owner) pid record is gone and NOTHING is claimed:
    /// the namespace is momentarily unowned and open to any starter. The socket
    /// has not been touched.
    StaleRecordCleared,
    /// This cleaner WON the heal claim — its own pid record is published, so
    /// every starter now reports `Taken`. The socket has not been touched.
    HealClaimHeld,
    /// The stale socket is gone; the heal claim is STILL published and still ours.
    SocketRemoved,
}

/// Self-heal a project's stale daemon artifacts after a failed attach (Fix A):
/// clears the pid lock AND removes the leftover `daemon.sock` at the RECORDED
/// (fallback-aware) socket path, so the next `serve --mcp` spawns a fresh daemon
/// instead of re-attaching to a dead socket that never answers.
///
/// Gated on liveness: returns `false` and touches nothing when the lock is held
/// by a LIVE pid (`clear_stale_daemon_lock` refuses to remove a live lock).
///
/// OWNERSHIP IS LOAD-BEARING — this cleaner unlinks a socket it did not bind, so
/// it must first BECOME the namespace's owner. The published pid record IS the
/// single-instance exclusion token ([`try_acquire_daemon_lock`] claims it with
/// `create_new`, and a daemon only ever binds after that claim succeeds). An
/// earlier revision cleared the stale record and THEN unlinked, holding no token
/// in between: a replacement daemon could legitimately claim the freed record and
/// bind the same path, and this unlink then destroyed the LIVE replacement's
/// socket — leaving a record naming a live daemon with no socket, which the
/// liveness gate makes unhealable. Merely swapping the two deletions does not fix
/// that, because an unlink performed while owning nothing is unsafe in either
/// order.
///
/// So the sequence is claim-then-clean, and every step after the liveness gate is
/// conditional on WINNING the claim:
///
/// 1. read the RECORDED socket (the record is the only place a fallback path is
///    written down), then remove the stale record — liveness-gated, so a live
///    owner's namespace is never entered;
/// 2. re-claim the pid record via [`try_acquire_daemon_lock`], i.e. with the very
///    same `create_new` token a starter uses. LOSING this claim means a
///    replacement (or a competing cleaner) now owns the namespace, so this cleaner
///    returns `false` and touches NOTHING — that is what closes the window rather
///    than narrowing it;
/// 3. unlink the stale socket while the claim excludes every starter, so no
///    process can have bound that path in the meantime;
/// 4. release the claim last, re-corroborated against our own pid
///    ([`cleanup_owned_lock`]).
///
/// An ABSENT pid record is treated as an unowned namespace: `clear_stale_daemon_lock`
/// reports it cleared, and step 2 then claims it, so the socket is still only ever
/// removed under a held claim. A crash mid-heal leaves a record naming this
/// now-dead process and no socket — exactly the self-healing residue the next
/// heal clears.
///
/// On Windows the recorded "socket path" is a bare namespaced pipe name rather
/// than a filesystem entry, so step 3's `remove_file` simply fails and is ignored
/// (a pipe disappears with its owning process); steps 1, 2 and 4 operate on the
/// real pid file and are what carry the exclusion there.
pub fn clear_stale_daemon_socket(project_root: &Path) -> bool {
    clear_stale_daemon_socket_with(project_root, |_| {})
}

fn clear_stale_daemon_socket_with(
    project_root: &Path,
    mut checkpoint: impl FnMut(StaleHealCheckpoint),
) -> bool {
    let Ok(pid_path) = daemon_pid_path(project_root) else {
        return false;
    };
    // Read the recorded socket BEFORE the stale record goes away: the record is
    // the only place a bind-fallback path is written down.
    let Ok(socket_path) = recorded_socket_path(project_root) else {
        return false;
    };
    // Liveness gate: only proceed once the owning pid is proven dead/absent.
    if !clear_stale_daemon_lock(&pid_path, None) {
        return false;
    }
    checkpoint(StaleHealCheckpoint::StaleRecordCleared);

    // Become the owner before deleting anything in the namespace. A lost claim
    // means somebody else owns it now; their socket is not ours to unlink.
    let claim_pid = match try_acquire_daemon_lock(project_root) {
        Ok(AcquireResult::Acquired { info, .. }) => info.pid,
        Ok(AcquireResult::Taken { .. }) => {
            debug!(
                pid_path = %pid_path.display(),
                "daemon rendezvous was re-claimed while healing; leaving its record and socket intact"
            );
            return false;
        }
        Err(err) => {
            debug!(
                pid_path = %pid_path.display(), error = %err,
                "could not claim the daemon rendezvous for healing; leaving it untouched"
            );
            return false;
        }
    };
    checkpoint(StaleHealCheckpoint::HealClaimHeld);

    // Under the claim no starter can have bound this path, so the socket we see
    // is provably the stale one.
    let _ = fs::remove_file(&socket_path);
    checkpoint(StaleHealCheckpoint::SocketRemoved);

    // Release the claim last, re-corroborated against our pid.
    cleanup_owned_lock(&pid_path, claim_pid)
}

/// Whether the published pid record currently names `pid`.
fn record_names(pid_path: &Path, pid: u32) -> bool {
    read_lock_info_tolerant(pid_path).is_some_and(|info| info.pid == pid)
}

pub(crate) fn cleanup_owned_lock(pid_path: &Path, pid: u32) -> bool {
    let owned = record_names(pid_path, pid);
    if owned {
        let _ = fs::remove_file(pid_path);
    }
    owned
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

/// The socket a client should connect to for `project_root` (`f83a1ec` /
/// D-Daemon-b): the path the daemon RECORDED in its lock, which is where it
/// actually bound after any bind-fallback. Falls back to the computed default
/// [`daemon_socket_path`] when the lock is absent, unreadable, or carries no
/// recorded socket (a legacy plain-pid lock). Reading the recorded path — not
/// recomputing — is what lets a client attach to a daemon that bound a fallback
/// candidate (e.g. the tmpdir socket on an ExFAT project dir).
pub fn recorded_socket_path(project_root: &Path) -> Result<PathBuf> {
    let pid_path = daemon_pid_path(project_root)?;
    match read_lock_info_tolerant(&pid_path)
        .map(|info| info.socket_path)
        .filter(|socket| !socket.as_os_str().is_empty())
    {
        Some(recorded) => Ok(recorded),
        None => daemon_socket_path(project_root),
    }
}

/// The two mutation boundaries of [`cleanup_owned_rendezvous`], exposed only so a
/// test can drive a competing daemon start at an exact point with no timing
/// assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendezvousCleanupCheckpoint {
    /// The pid record was just corroborated as ours and is STILL PUBLISHED. The
    /// socket has not been touched.
    OwnershipCorroborated,
    /// Our socket is gone; the pid record is still published and still ours.
    SocketRemoved,
}

/// Remove the rendezvous artifacts this daemon process itself published: on Unix
/// its bound socket file, then its owner-bound pid record.
///
/// ORDER IS LOAD-BEARING. The published pid record IS the single-instance
/// exclusion: `try_acquire_daemon_lock` claims it with `create_new`, so while our
/// record exists a competing start reports `Taken` and never binds this socket
/// path. Removing the record FIRST (as an earlier revision did) opens a window in
/// which a replacement daemon legitimately claims the record and binds the same
/// path — and the departing process's later unlink then destroys the LIVE
/// daemon's socket while its record still advertises it. So the record is held as
/// exclusion until our socket is unlinked, and only then is the record removed,
/// re-corroborated against our pid.
///
/// Crash behavior between the two operations is fail-closed and self-healing: the
/// namespace is left with a pid record naming a now-dead pid and no socket. A
/// client's attach to the recorded socket fails fast (bounded, never hangs), and
/// `clear_stale_daemon_lock` / `clear_stale_daemon_socket` clear that record on the
/// next start because the recorded pid is provably not alive. The inverse order
/// could instead leave a LIVE daemon with no socket, which nothing can heal.
///
/// An already-replaced record (an owner mismatch observed on entry) preserves BOTH
/// the replacement record and its socket and reports `false`. The permanent index
/// lock is never named here.
pub(crate) fn cleanup_owned_rendezvous(pid_path: &Path, socket_path: &Path, pid: u32) -> bool {
    cleanup_owned_rendezvous_with(pid_path, socket_path, pid, |_| {})
}

fn cleanup_owned_rendezvous_with(
    pid_path: &Path,
    socket_path: &Path,
    pid: u32,
    mut checkpoint: impl FnMut(RendezvousCleanupCheckpoint),
) -> bool {
    if !record_names(pid_path, pid) {
        debug!(
            pid_path = %pid_path.display(),
            "daemon rendezvous is owned by another process; leaving its record and socket intact"
        );
        return false;
    }
    checkpoint(RendezvousCleanupCheckpoint::OwnershipCorroborated);

    #[cfg(unix)]
    if let Some(stale) = crate::transport::Rendezvous::from_socket_path(socket_path).cleanup_path()
    {
        let _ = fs::remove_file(stale);
    }
    #[cfg(not(unix))]
    let _ = socket_path;
    checkpoint(RendezvousCleanupCheckpoint::SocketRemoved);

    // Re-corroborate at the second mutation boundary: only a record that STILL
    // names this process may be removed.
    cleanup_owned_lock(pid_path, pid)
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

    #[test]
    fn rewrite_socket_path_updates_recorded_socket_and_keeps_pid() {
        // Given an acquired lock recording the default socket, rewriting the
        // recorded socket to a fallback path is what the client later reads.
        let base = temp_base("rewrite");
        let AcquireResult::Acquired { pid_path, info } =
            try_acquire_daemon_lock(&base).expect("acquire lock")
        else {
            panic!("expected a fresh lock to be acquired");
        };

        let fallback = std::env::temp_dir().join("codegraph-fallback.sock");
        rewrite_lock_socket_path(&pid_path, &fallback).expect("rewrite socket path");

        let raw = fs::read_to_string(&pid_path).expect("read lock");
        let reloaded = decode_lock_info(&raw).expect("decode lock");
        assert_eq!(reloaded.socket_path, fallback);
        assert_eq!(reloaded.pid, info.pid);
        assert_eq!(reloaded.version, info.version);

        let _ = fs::remove_dir_all(&base);
    }

    /// A real, existing project directory. `IndexPaths` derives the physical
    /// project identity from the filesystem object, so the project must exist
    /// before any rendezvous path resolves.
    fn temp_base(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "cg-lock-{label}-{}-{}",
            process::id(),
            now_millis()
        ));
        fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    fn pid_path_of(base: &Path) -> PathBuf {
        daemon_pid_path(base).expect("resolve the v2 rendezvous pid path")
    }

    fn socket_path_of(base: &Path) -> PathBuf {
        daemon_socket_path(base).expect("resolve the v2 rendezvous socket identity")
    }

    fn create_rendezvous_dir(base: &Path) {
        let dir = rendezvous_dir(base).expect("resolve the v2 rendezvous dir");
        fs::create_dir_all(dir).unwrap();
    }

    /// A bound socket is a FILESYSTEM artifact only on unix. On windows
    /// [`daemon_socket_path`] returns a bare namespaced pipe NAME
    /// (`codegraph-v2-<hash16>`), a kernel object reclaimed when its last handle
    /// closes — which is why [`cleanup_owned_rendezvous`] unlinks nothing there,
    /// correctly. Writing that name as a file and then asserting it was removed
    /// tests a premise windows does not have, so these helpers confine the
    /// file-level claims to unix. The ORDERING guarantees stay asserted on EVERY
    /// platform: the pid record as exclusion, a competing start refused at the
    /// midpoint, and the crash residue the stale heal clears.
    fn publish_socket_file(socket: &Path, contents: &[u8]) {
        #[cfg(unix)]
        fs::write(socket, contents).unwrap();
        #[cfg(not(unix))]
        let _ = (socket, contents);
    }

    fn assert_socket_file_present(socket: &Path, message: &str) {
        #[cfg(unix)]
        assert!(socket.exists(), "{message}");
        #[cfg(not(unix))]
        let _ = (socket, message);
    }

    fn assert_socket_file_absent(socket: &Path, message: &str) {
        #[cfg(unix)]
        assert!(!socket.exists(), "{message}");
        #[cfg(not(unix))]
        let _ = (socket, message);
    }

    #[test]
    fn decode_lock_info_rejects_empty_and_zero_and_garbage() {
        assert!(decode_lock_info("").is_none());
        assert!(decode_lock_info("   \n").is_none());
        assert!(decode_lock_info("0").is_none());
        assert!(decode_lock_info("not-a-pid").is_none());
    }

    #[test]
    fn encode_then_decode_round_trips_full_lock_info() {
        let info = DaemonLockInfo {
            pid: 4242,
            version: "9.9.9".to_string(),
            socket_path: PathBuf::from("/tmp/x.sock"),
            started_at: 1_700_000_000_000,
        };
        let encoded = encode_lock_info(&info).unwrap();
        assert!(encoded.ends_with('\n'));
        assert_eq!(decode_lock_info(&encoded), Some(info));
    }

    #[test]
    fn clear_stale_lock_returns_true_when_missing() {
        let base = temp_base("clear-missing");
        let pid_path = pid_path_of(&base);
        assert!(clear_stale_daemon_lock(&pid_path, None));
    }

    #[test]
    fn clear_stale_lock_refuses_to_remove_a_live_owned_lock() {
        let base = temp_base("clear-live");
        let AcquireResult::Acquired { pid_path, .. } =
            try_acquire_daemon_lock(&base).expect("acquire")
        else {
            panic!("expected fresh acquire");
        };
        assert!(
            !clear_stale_daemon_lock(&pid_path, None),
            "a lock held by this live pid must never be cleared"
        );
        assert!(pid_path.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_stale_lock_removes_a_dead_pid_lock() {
        let base = temp_base("clear-dead");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let dead = DaemonLockInfo {
            pid: 4_000_000_000,
            version: "1.0.0".to_string(),
            socket_path: PathBuf::from("/tmp/dead.sock"),
            started_at: 0,
        };
        fs::write(&pid_path, encode_lock_info(&dead).unwrap()).unwrap();
        assert!(clear_stale_daemon_lock(&pid_path, None));
        assert!(!pid_path.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_stale_lock_refuses_when_expected_pid_mismatches() {
        let base = temp_base("clear-mismatch");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let dead = DaemonLockInfo {
            pid: 4_000_000_000,
            version: "1.0.0".to_string(),
            socket_path: PathBuf::new(),
            started_at: 0,
        };
        fs::write(&pid_path, encode_lock_info(&dead).unwrap()).unwrap();
        assert!(
            !clear_stale_daemon_lock(&pid_path, Some(12345)),
            "an expected-dead-pid that mismatches the lock must not delete it"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unlock_project_clears_a_dead_lock() {
        let base = temp_base("unlock");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let dead = DaemonLockInfo {
            pid: 4_000_000_000,
            version: "1.0.0".to_string(),
            socket_path: PathBuf::new(),
            started_at: 0,
        };
        fs::write(&pid_path, encode_lock_info(&dead).unwrap()).unwrap();
        assert!(unlock_project(&base));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn recorded_socket_path_falls_back_to_default_when_lock_absent() {
        let base = temp_base("recorded-absent");
        assert_eq!(
            recorded_socket_path(&base).expect("resolve recorded socket"),
            socket_path_of(&base)
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn recorded_socket_path_reads_the_recorded_socket_from_the_lock() {
        let base = temp_base("recorded-present");
        let AcquireResult::Acquired { pid_path, .. } =
            try_acquire_daemon_lock(&base).expect("acquire")
        else {
            panic!("expected fresh acquire");
        };
        let recorded = std::env::temp_dir().join("cg-recorded.sock");
        rewrite_lock_socket_path(&pid_path, &recorded).unwrap();
        assert_eq!(
            recorded_socket_path(&base).expect("resolve recorded socket"),
            recorded
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn try_acquire_reports_taken_when_lock_held_by_live_pid() {
        let base = temp_base("taken");
        let AcquireResult::Acquired { .. } = try_acquire_daemon_lock(&base).expect("first acquire")
        else {
            panic!("first acquire should succeed");
        };
        match try_acquire_daemon_lock(&base).expect("second acquire") {
            AcquireResult::Taken { existing, .. } => {
                let info = existing.expect("existing lock info present");
                assert_eq!(info.pid, process::id());
            }
            AcquireResult::Acquired { .. } => panic!("second acquire must report Taken"),
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cleanup_owned_rendezvous_preserves_a_replacement_owners_record_and_socket() {
        let base = temp_base("cleanup-replaced");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        // A NEW owner already replaced the record and rebound the same socket.
        let replacement = DaemonLockInfo {
            pid: process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            socket_path: socket.clone(),
            started_at: 2,
        };
        fs::write(&pid_path, encode_lock_info(&replacement).unwrap()).unwrap();
        fs::write(&socket, b"").unwrap();

        // The DEPARTING daemon (a different pid) must strip neither artifact.
        let mut departing = 4_000_000_000u32;
        while departing == process::id() {
            departing -= 1;
        }
        assert!(
            !cleanup_owned_rendezvous(&pid_path, &socket, departing),
            "an owner mismatch must report that nothing was cleaned"
        );
        assert!(
            pid_path.exists(),
            "the replacement owner's record must survive"
        );
        assert!(
            socket.exists(),
            "the replacement owner's socket must survive"
        );

        // The true owner cleans both.
        assert!(cleanup_owned_rendezvous(&pid_path, &socket, process::id()));
        assert!(!pid_path.exists());
        #[cfg(unix)]
        assert!(!socket.exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// The former vulnerable midpoint. A competing start is driven at the EXACT
    /// boundary where the old revision had already removed the pid record, and it
    /// is allowed to claim the record and rebind the same socket path. The
    /// departing owner must not destroy that replacement.
    ///
    /// Deterministic by construction: the replacement runs inside the checkpoint
    /// callback, so it is ordered by the call itself, not by any sleep.
    ///
    /// Under the OLD pid-record-first order this test fails — `try_acquire` would
    /// succeed (the record was gone), and the subsequent unlink would delete the
    /// replacement's socket.
    #[test]
    fn a_replacement_start_at_the_cleanup_midpoint_keeps_its_own_rendezvous() {
        let base = temp_base("cleanup-midpoint");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);

        // The DEPARTING owner: a distinct, provably dead pid, with its record
        // published and its socket bound.
        let mut departing = 4_000_000_000u32;
        while departing == process::id() || is_process_alive(departing) {
            departing -= 1;
        }
        let old = DaemonLockInfo {
            pid: departing,
            version: env!("CARGO_PKG_VERSION").to_string(),
            socket_path: socket.clone(),
            started_at: 1,
        };
        fs::write(&pid_path, encode_lock_info(&old).unwrap()).unwrap();
        publish_socket_file(&socket, b"old");

        let mut replacement_claim = None;
        let cleaned = cleanup_owned_rendezvous_with(&pid_path, &socket, departing, |checkpoint| {
            if checkpoint != RendezvousCleanupCheckpoint::OwnershipCorroborated {
                return;
            }
            // A competing start races here. While OUR record is still published it
            // must be refused, which is exactly what keeps the replacement from
            // binding a socket the departing owner is about to unlink.
            replacement_claim = Some(try_acquire_daemon_lock(&base).expect("competing start"));
        });

        match replacement_claim.expect("the competing start ran at the checkpoint") {
            AcquireResult::Taken { existing, .. } => {
                let existing = existing.expect("the departing record is readable");
                assert_eq!(
                    existing.pid, departing,
                    "the published record must still be the departing owner's, so the \
                     replacement is refused instead of binding a doomed socket"
                );
            }
            AcquireResult::Acquired { .. } => panic!(
                "a competing start must NOT be able to claim the record before the departing \
                 owner has finished unlinking its socket"
            ),
        }

        assert!(cleaned, "the departing owner cleans its own rendezvous");
        assert!(!pid_path.exists(), "its record is removed last");
        assert_socket_file_absent(&socket, "its socket was removed first");

        // The replacement can now claim a clean namespace and bind its own socket,
        // and nothing left over can delete it.
        let AcquireResult::Acquired { .. } =
            try_acquire_daemon_lock(&base).expect("post-cleanup acquire")
        else {
            panic!("a cleaned namespace must be claimable");
        };
        publish_socket_file(&socket, b"new");
        assert!(
            !cleanup_owned_rendezvous(&pid_path, &socket, departing),
            "a second cleanup pass by the departed owner must be refused"
        );
        assert!(pid_path.exists());
        assert_socket_file_present(&socket, "the replacement's socket survives");
        let _ = fs::remove_dir_all(&base);
    }

    /// The socket is unlinked BEFORE the record, so a crash between the two
    /// boundaries leaves a dead-pid record with no socket — the self-healing
    /// residue `clear_stale_daemon_socket` is built to clear. The inverse order
    /// would leave a live daemon with no socket, which nothing can heal.
    #[test]
    fn a_crash_between_cleanup_boundaries_leaves_only_self_healing_residue() {
        let base = temp_base("cleanup-crash");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        let mut departing = 4_000_000_000u32;
        while departing == process::id() || is_process_alive(departing) {
            departing -= 1;
        }
        let old = DaemonLockInfo {
            pid: departing,
            version: env!("CARGO_PKG_VERSION").to_string(),
            socket_path: socket.clone(),
            started_at: 1,
        };
        fs::write(&pid_path, encode_lock_info(&old).unwrap()).unwrap();
        publish_socket_file(&socket, b"old");

        // Simulate the crash by panicking-free early return: run cleanup only up to
        // the socket-removed boundary and drop the process there.
        let mut reached_socket_removed = false;
        let _ = cleanup_owned_rendezvous_with(&pid_path, &socket, departing, |checkpoint| {
            if checkpoint == RendezvousCleanupCheckpoint::SocketRemoved {
                reached_socket_removed = true;
                assert_socket_file_absent(&socket, "the socket is gone at this boundary");
                assert!(
                    pid_path.exists(),
                    "the record is STILL published as exclusion at this boundary"
                );
            }
        });
        assert!(reached_socket_removed);

        // The residue is exactly what the stale-socket self-heal clears.
        assert!(clear_stale_daemon_socket(&base));
        assert!(!pid_path.exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// A dead pid whose record and socket are the stale residue to be healed.
    fn dead_pid() -> u32 {
        let mut candidate = 4_000_000_000u32;
        while candidate == process::id() || is_process_alive(candidate) {
            candidate -= 1;
        }
        candidate
    }

    fn publish_record(pid_path: &Path, pid: u32, socket: &Path) {
        let info = DaemonLockInfo {
            pid,
            version: env!("CARGO_PKG_VERSION").to_string(),
            socket_path: socket.to_path_buf(),
            started_at: 1,
        };
        fs::write(pid_path, encode_lock_info(&info).unwrap()).unwrap();
    }

    /// The rendezvous-ownership race. A replacement daemon legitimately claims the
    /// namespace at the EXACT point where the stale record is gone and nothing is
    /// owned, and binds the same socket path. The cleaner must lose its claim and
    /// destroy nothing.
    ///
    /// Deterministic by construction: the replacement runs inside the checkpoint
    /// callback, so it is ordered by the call itself, not by any sleep.
    ///
    /// Under the OLD clear-record-then-unlink order this test fails — the unlink
    /// was unconditional, so it deleted the LIVE replacement's socket.
    #[test]
    fn a_replacement_start_during_the_stale_heal_keeps_its_own_socket() {
        let base = temp_base("heal-replaced");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        publish_record(&pid_path, dead_pid(), &socket);
        fs::write(&socket, b"stale").unwrap();

        let mut replacement_ran = false;
        let healed = clear_stale_daemon_socket_with(&base, |checkpoint| {
            if checkpoint != StaleHealCheckpoint::StaleRecordCleared {
                return;
            }
            // A replacement start wins the freed namespace here: it claims the
            // record with its own LIVE pid and binds the same socket path.
            replacement_ran = true;
            publish_record(&pid_path, process::id(), &socket);
            fs::write(&socket, b"live-replacement").unwrap();
        });

        assert!(replacement_ran, "the replacement ran at the checkpoint");
        assert!(
            socket.exists(),
            "the LIVE replacement's socket must survive the stale heal"
        );
        assert_eq!(
            fs::read(&socket).unwrap(),
            b"live-replacement",
            "the surviving socket must be the replacement's, not the stale one"
        );
        assert!(
            !healed,
            "a cleaner that lost the namespace must report that it healed nothing"
        );
        let surviving =
            read_lock_info_tolerant(&pid_path).expect("the replacement record survives");
        assert_eq!(
            surviving.pid,
            process::id(),
            "the replacement's record must be left exactly as published"
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// The claim is what makes the unlink safe, so it must be PUBLISHED across the
    /// unlink and released only afterwards. While it is held every starter is
    /// excluded, which is why no process can have bound the path being unlinked.
    #[test]
    fn the_stale_heal_holds_its_claim_across_the_unlink_and_releases_it_last() {
        let base = temp_base("heal-claim");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        publish_record(&pid_path, dead_pid(), &socket);
        fs::write(&socket, b"stale").unwrap();

        let mut competing_start = None;
        let mut saw_socket_removed = false;
        let healed = clear_stale_daemon_socket_with(&base, |checkpoint| match checkpoint {
            StaleHealCheckpoint::HealClaimHeld => {
                let claim =
                    read_lock_info_tolerant(&pid_path).expect("the heal claim is published");
                assert_eq!(
                    claim.pid,
                    process::id(),
                    "the heal claim must name this cleaner"
                );
                assert!(socket.exists(), "the socket is untouched under the claim");
                competing_start =
                    Some(try_acquire_daemon_lock(&base).expect("competing start under the claim"));
            }
            StaleHealCheckpoint::SocketRemoved => {
                saw_socket_removed = true;
                assert!(
                    !socket.exists(),
                    "the stale socket is gone at this boundary"
                );
                assert!(
                    pid_path.exists(),
                    "the heal claim must STILL be published while the socket is unlinked"
                );
            }
            StaleHealCheckpoint::StaleRecordCleared => {}
        });

        match competing_start.expect("a competing start ran under the claim") {
            AcquireResult::Taken { existing, .. } => {
                let existing = existing.expect("the heal claim is readable");
                assert_eq!(
                    existing.pid,
                    process::id(),
                    "a starter racing the heal must be refused BY the heal claim, so it \
                     cannot bind the socket the cleaner is about to unlink"
                );
            }
            AcquireResult::Acquired { .. } => panic!(
                "a starter must NOT be able to claim the namespace while the cleaner holds \
                 its heal claim"
            ),
        }
        assert!(saw_socket_removed);
        assert!(healed, "a genuinely stale namespace heals");
        assert!(!pid_path.exists(), "the heal claim is released last");
        let _ = fs::remove_dir_all(&base);
    }

    /// Stale recovery still works: a dead owner's record AND socket are both gone
    /// after the heal, so the next start spawns fresh.
    #[test]
    fn a_genuinely_stale_namespace_still_self_heals_both_artifacts() {
        let base = temp_base("heal-stale");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        publish_record(&pid_path, dead_pid(), &socket);
        fs::write(&socket, b"stale").unwrap();

        assert!(clear_stale_daemon_socket(&base), "the stale residue heals");
        assert!(!pid_path.exists(), "the stale record is gone");
        assert!(!socket.exists(), "the stale socket is gone");
        let AcquireResult::Acquired { .. } =
            try_acquire_daemon_lock(&base).expect("post-heal acquire")
        else {
            panic!("a healed namespace must be claimable by a fresh start");
        };
        let _ = fs::remove_dir_all(&base);
    }

    /// An ABSENT record means nobody owns the namespace, so a leftover socket may
    /// be removed — but only after the cleaner has CLAIMED that namespace, exactly
    /// as it does for a dead-owner record. Without the claim, an absent record
    /// would be the widest interleaving of all: it names no pid to prove dead, so
    /// an unlink under it could hit a daemon that bound between the two syscalls.
    #[test]
    fn an_absent_record_is_claimed_before_its_leftover_socket_is_removed() {
        let base = temp_base("heal-absent");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        fs::write(&socket, b"orphan").unwrap();
        assert!(!pid_path.exists(), "precondition: no record at all");

        let mut claim_held_with_socket_present = false;
        let healed = clear_stale_daemon_socket_with(&base, |checkpoint| {
            if checkpoint == StaleHealCheckpoint::HealClaimHeld {
                claim_held_with_socket_present =
                    record_names(&pid_path, process::id()) && socket.exists();
            }
        });

        assert!(
            claim_held_with_socket_present,
            "the leftover socket must still be present at the moment the claim is won, \
             proving the removal happens UNDER the claim and not before it"
        );
        assert!(healed, "an unowned namespace with a leftover socket heals");
        assert!(!socket.exists(), "the orphaned socket is removed");
        assert!(!pid_path.exists(), "the heal claim is released");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_stale_daemon_socket_removes_lock_and_socket_when_dead() {
        let base = temp_base("socket-dead");
        create_rendezvous_dir(&base);
        let pid_path = pid_path_of(&base);
        let socket = socket_path_of(&base);
        let dead = DaemonLockInfo {
            pid: 4_000_000_000,
            version: "1.0.0".to_string(),
            socket_path: socket.clone(),
            started_at: 0,
        };
        fs::write(&pid_path, encode_lock_info(&dead).unwrap()).unwrap();
        let _ = fs::write(&socket, b"");
        assert!(clear_stale_daemon_socket(&base));
        assert!(!pid_path.exists());
        let _ = fs::remove_dir_all(&base);
    }
}
