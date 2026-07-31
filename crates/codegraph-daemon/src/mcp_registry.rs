//! Global, PID-keyed registry for foreground stdio MCP processes.
//!
//! `serve --mcp` speaks MCP over stdio in the FOREGROUND — the process a user
//! actually sees in their process list. Unlike the per-project daemon (keyed by
//! `.codegraph-v2/daemon.pid` under the project root) and unlike an HTTP MCP
//! server (keyed by bind addr, see [`crate::http_registry`]), a stdio process
//! has no natural rendezvous of its own: several may serve the same project at
//! once, and one may serve no resolvable project at all. So it is keyed by PID,
//! one `<pid>.json` per process, in a GLOBAL state directory.
//!
//! This registry is PURE OBSERVABILITY — "who is running, since when, against
//! which project". It deliberately offers no terminate path: a PID whose entry
//! outlived a crash may have been reused by an unrelated process, and this
//! crate has no portable way to prove instance identity, so killing by
//! registered PID could hit an innocent process. `project` / `started_at` /
//! `version` exist so a HUMAN can recognize a stale row instead.
//!
//! Registry directory resolution (mirrors [`crate::http_registry`], with an
//! `mcp` leaf so the two registries never share a directory):
//!   1. `CODEGRAPH_MCP_REGISTRY_DIR` (explicit override — used by tests and
//!      power users);
//!   2. `XDG_STATE_HOME/codegraph/mcp` when `XDG_STATE_HOME` is set;
//!   3. `$HOME/.local/state/codegraph/mcp` (unix / XDG fallback);
//!   4. `%LOCALAPPDATA%\codegraph\mcp` (windows), falling back to `USERPROFILE`.
//!
//! Reads distinguish "nobody registered yet" from "the registry could not be
//! read" — see [`RegistryRead`]. A MISSING directory is the former: it is the
//! normal state before the first `serve --mcp` ever runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::process::is_process_alive;

/// Explicit override for the registry directory (highest precedence). Tests set
/// this to an isolated temp dir so they never touch a developer's real state.
pub const CODEGRAPH_MCP_REGISTRY_DIR: &str = "CODEGRAPH_MCP_REGISTRY_DIR";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project: Option<String>,
    pub transport: String,
    pub started_at: u64,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryRead {
    Available(Vec<McpServerInfo>),
    Unavailable { path: PathBuf, error: String },
}

/// Resolve the global registry directory (see module docs for precedence).
/// Does NOT create it — call [`ensure_registry_dir`] for that.
#[must_use]
pub fn registry_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os(CODEGRAPH_MCP_REGISTRY_DIR) {
        let raw = PathBuf::from(explicit);
        if !raw.as_os_str().is_empty() {
            return raw;
        }
    }
    base_state_dir().join("codegraph").join("mcp")
}

#[cfg(unix)]
fn base_state_dir() -> PathBuf {
    if let Some(xdg) = non_empty_env("XDG_STATE_HOME") {
        return PathBuf::from(xdg);
    }
    if let Some(home) = non_empty_env("HOME") {
        return PathBuf::from(home).join(".local").join("state");
    }
    // Last resort: a temp-dir bucket so we never write to `/`.
    std::env::temp_dir().join("codegraph-state")
}

#[cfg(windows)]
fn base_state_dir() -> PathBuf {
    if let Some(local) = non_empty_env("LOCALAPPDATA") {
        return PathBuf::from(local);
    }
    if let Some(profile) = non_empty_env("USERPROFILE") {
        return PathBuf::from(profile).join("AppData").join("Local");
    }
    std::env::temp_dir().join("codegraph-state")
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Create the registry directory (idempotent) and return it.
pub fn ensure_registry_dir() -> Result<PathBuf> {
    let dir = registry_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating MCP registry dir {}", dir.display()))?;
    Ok(dir)
}

/// Absolute path of the registry file for `pid` under `dir`.
#[must_use]
pub fn registry_file(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

/// Current epoch milliseconds (0 on a pre-1970 clock, never panics).
#[must_use]
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Write (create/overwrite) the registry entry for `info.pid`. Creates the
/// registry dir if needed. Atomic-ish: writes a temp file then renames over the
/// final path so a concurrent reader never sees a partial record.
pub fn write_entry(info: &McpServerInfo) -> Result<PathBuf> {
    let dir = ensure_registry_dir()?;
    let path = registry_file(&dir, info.pid);
    let payload = format!("{}\n", serde_json::to_string_pretty(info)?);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, &payload).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("publishing {}", path.display()))?;
    Ok(path)
}

/// Read a single registry entry by pid, if present and parseable.
#[must_use]
pub fn read_entry(pid: u32) -> Option<McpServerInfo> {
    read_entry_file(&registry_file(&registry_dir(), pid))
}

fn read_entry_file(path: &Path) -> Option<McpServerInfo> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<McpServerInfo>(raw.trim()).ok()
}

/// What a directory scan found. `Missing` is NOT a failure: no registry
/// directory means no stdio MCP process has ever registered, which reads as an
/// empty-but-available registry.
enum DirScan {
    Missing,
    Files(Vec<PathBuf>),
    Failed(String),
}

fn scan_registry_dir(dir: &Path) -> DirScan {
    match fs::read_dir(dir) {
        Ok(read) => {
            let mut files: Vec<PathBuf> = read
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            files.sort();
            DirScan::Files(files)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DirScan::Missing,
        // Anything else (a FILE where the dir should be, a permission denial, an
        // unreadable mount) is a genuine outage the caller must be able to tell
        // apart from "nothing registered".
        Err(error) => DirScan::Failed(error.to_string()),
    }
}

/// List EVERY registry entry (live or stale) without pruning. Sorted by pid for
/// deterministic output.
#[must_use]
pub fn list_entries() -> RegistryRead {
    let dir = registry_dir();
    match scan_registry_dir(&dir) {
        DirScan::Missing => RegistryRead::Available(Vec::new()),
        DirScan::Failed(error) => RegistryRead::Unavailable { path: dir, error },
        DirScan::Files(files) => {
            let mut entries: Vec<McpServerInfo> = files
                .iter()
                .filter_map(|path| read_entry_file(path))
                .collect();
            entries.sort_by_key(|entry| entry.pid);
            RegistryRead::Available(entries)
        }
    }
}

/// Prune every registry entry whose pid is no longer alive (self-heal), plus any
/// unparseable file. Returns the pids that were pruned, sorted. A live entry is
/// never touched. `Err` means the registry directory itself could not be read.
pub fn prune_dead() -> Result<Vec<u32>> {
    let dir = registry_dir();
    let files = match scan_registry_dir(&dir) {
        DirScan::Missing => return Ok(Vec::new()),
        DirScan::Failed(error) => {
            return Err(anyhow!(
                "reading MCP registry dir {}: {error}",
                dir.display()
            ));
        }
        DirScan::Files(files) => files,
    };
    let mut pruned = Vec::new();
    for path in files {
        match read_entry_file(&path) {
            Some(info) if is_process_alive(info.pid) => {}
            Some(info) => {
                if fs::remove_file(&path).is_ok() {
                    pruned.push(info.pid);
                }
            }
            None => {
                // Unparseable/corrupt file: remove it so it stops shadowing a
                // future healthy entry for the same pid.
                let _ = fs::remove_file(&path);
            }
        }
    }
    pruned.sort_unstable();
    Ok(pruned)
}

/// List entries after pruning dead ones — the canonical "what is running now".
#[must_use]
pub fn live_entries() -> RegistryRead {
    // Pruning is best-effort here: [`list_entries`] below owns the
    // Available-vs-Unavailable classification, so an unreadable registry is
    // reported once, from one place, instead of twice with two error strings.
    let _ = prune_dead();
    list_entries()
}

/// Remove the registry entry for `pid` (best-effort; a missing file is already
/// the desired end state). Returns true when a file was removed.
pub fn remove_entry(pid: u32) -> bool {
    fs::remove_file(registry_file(&registry_dir(), pid)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::is_process_alive;
    use std::fs;
    use std::sync::{Mutex, MutexGuard};
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempRegistry {
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl TempRegistry {
        fn new(label: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "cg-mcp-reg-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os(CODEGRAPH_MCP_REGISTRY_DIR);
            // SAFETY: guarded by ENV_LOCK for the guard's lifetime.
            unsafe { std::env::set_var(CODEGRAPH_MCP_REGISTRY_DIR, &dir) };
            Self {
                dir,
                _guard: guard,
                previous,
            }
        }
    }

    impl Drop for TempRegistry {
        fn drop(&mut self) {
            // SAFETY: guarded by ENV_LOCK for the guard's lifetime.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(CODEGRAPH_MCP_REGISTRY_DIR, value),
                    None => std::env::remove_var(CODEGRAPH_MCP_REGISTRY_DIR),
                }
            }
            if self.dir.is_dir() {
                let _ = fs::remove_dir_all(&self.dir);
            } else {
                let _ = fs::remove_file(&self.dir);
            }
        }
    }

    fn sample(pid: u32) -> McpServerInfo {
        McpServerInfo {
            pid,
            project: Some("/work/project".to_string()),
            transport: "stdio".to_string(),
            started_at: 1_700_000_000_123,
            version: "1.2.3-test".to_string(),
        }
    }

    fn available(result: RegistryRead) -> Vec<McpServerInfo> {
        match result {
            RegistryRead::Available(entries) => entries,
            RegistryRead::Unavailable { path, error } => {
                panic!(
                    "registry unexpectedly unavailable at {}: {error}",
                    path.display()
                )
            }
        }
    }

    fn pick_dead_pid() -> u32 {
        let candidate = 4_000_000_000u32;
        if is_process_alive(candidate) {
            0
        } else {
            candidate
        }
    }

    #[test]
    fn write_then_read_roundtrip_preserves_every_field() {
        let _registry = TempRegistry::new("roundtrip");
        let info = sample(std::process::id());

        write_entry(&info).unwrap();

        assert_eq!(read_entry(info.pid), Some(info));
    }

    #[test]
    fn live_entries_prunes_an_entry_whose_pid_is_dead() {
        let registry = TempRegistry::new("dead");
        let dead_pid = pick_dead_pid();
        write_entry(&sample(dead_pid)).unwrap();

        assert!(available(live_entries()).is_empty());
        assert!(!registry_file(&registry.dir, dead_pid).exists());
    }

    #[test]
    fn corrupt_pid_json_does_not_poison_the_whole_list() {
        let registry = TempRegistry::new("corrupt");
        let live = sample(std::process::id());
        write_entry(&live).unwrap();
        let corrupt = registry.dir.join("424242.json");
        fs::write(&corrupt, b"{ not valid json").unwrap();

        assert_eq!(available(live_entries()), vec![live]);
        assert!(!corrupt.exists(), "corrupt registry file must be pruned");
    }

    #[test]
    fn missing_registry_directory_reads_as_available_and_empty() {
        let registry = TempRegistry::new("missing");
        fs::remove_dir_all(&registry.dir).unwrap();

        assert_eq!(live_entries(), RegistryRead::Available(Vec::new()));
    }

    #[test]
    fn unreadable_registry_path_reads_as_unavailable_not_empty() {
        let registry = TempRegistry::new("unavailable");
        fs::remove_dir_all(&registry.dir).unwrap();
        fs::write(&registry.dir, b"not a directory").unwrap();

        match live_entries() {
            RegistryRead::Unavailable { path, error } => {
                assert_eq!(path, registry.dir);
                assert!(!error.is_empty());
            }
            RegistryRead::Available(entries) => {
                panic!("unreadable registry was reported as available: {entries:?}")
            }
        }
    }

    #[test]
    fn remove_entry_deletes_the_pid_file() {
        let _registry = TempRegistry::new("remove");
        let pid = std::process::id();
        write_entry(&sample(pid)).unwrap();

        assert!(remove_entry(pid));
        assert_eq!(read_entry(pid), None);
    }
}
