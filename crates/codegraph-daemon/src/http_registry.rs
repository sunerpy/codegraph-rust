//! Global, addr-keyed registry for background HTTP MCP servers.
//!
//! Unlike the per-project stdio daemon (keyed by `.codegraph/daemon.pid` under
//! the project root), an HTTP MCP server started with `serve --http` is keyed
//! by its BIND ADDRESS: in global mode (no `--path`) one server spans many
//! projects, so its lifecycle cannot live inside any single `.codegraph/`. This
//! module owns a GLOBAL state directory with one JSON file per running server,
//! plus liveness-gated self-healing (a dead pid's entry is pruned on read).
//!
//! Registry directory resolution (dependency-light, no `dirs` crate):
//!   1. `CODEGRAPH_HTTP_REGISTRY_DIR` (explicit override — used by tests and
//!      power users);
//!   2. `XDG_STATE_HOME/codegraph/http` when `XDG_STATE_HOME` is set;
//!   3. `$HOME/.local/state/codegraph/http` (unix / XDG fallback);
//!   4. `%LOCALAPPDATA%\codegraph\http` (windows), falling back to `USERPROFILE`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::process::is_process_alive;

/// Explicit override for the registry directory (highest precedence). Tests set
/// this to an isolated temp dir so they never touch a developer's real state.
pub const CODEGRAPH_HTTP_REGISTRY_DIR: &str = "CODEGRAPH_HTTP_REGISTRY_DIR";

/// Bind mode of a running HTTP MCP server: `pinned` (started with `--path`,
/// one project) or `global` (no `--path`, per-call `projectPath`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMode {
    Pinned,
    Global,
}

impl HttpMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMode::Pinned => "pinned",
            HttpMode::Global => "global",
        }
    }
}

/// One registry record — the on-disk `<addr-sanitized>.json` payload describing
/// a single running HTTP MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpServerInfo {
    pub pid: u32,
    pub addr: String,
    pub mode: HttpMode,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project: Option<String>,
    /// Epoch milliseconds when the server started.
    pub started_at: u64,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub log_file: Option<String>,
}

/// Resolve the global registry directory (see module docs for precedence).
/// Does NOT create it — call [`ensure_registry_dir`] for that.
#[must_use]
pub fn registry_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os(CODEGRAPH_HTTP_REGISTRY_DIR) {
        let raw = PathBuf::from(explicit);
        if !raw.as_os_str().is_empty() {
            return raw;
        }
    }
    base_state_dir().join("codegraph").join("http")
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
        .with_context(|| format!("creating HTTP registry dir {}", dir.display()))?;
    Ok(dir)
}

/// Sanitize a bind address into a filesystem-safe stem: `:` and `/` (and any
/// other separator) become `_`, so `0.0.0.0:12025` → `0.0.0.0_12025` and
/// `[::1]:8111` → `[__1]_8111`. Deterministic and reversible-enough for humans.
#[must_use]
pub fn sanitize_addr(addr: &str) -> String {
    addr.chars()
        .map(|c| match c {
            ':' | '/' | '\\' => '_',
            other => other,
        })
        .collect()
}

/// Absolute path of the registry file for `addr` under `dir`.
#[must_use]
pub fn registry_file(dir: &Path, addr: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_addr(addr)))
}

/// Current epoch milliseconds (0 on a pre-1970 clock, never panics).
#[must_use]
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Write (create/overwrite) the registry entry for `info.addr`. Creates the
/// registry dir if needed. Atomic-ish: writes a temp file then renames over the
/// final path so a concurrent reader never sees a partial record.
pub fn write_entry(info: &HttpServerInfo) -> Result<PathBuf> {
    let dir = ensure_registry_dir()?;
    let path = registry_file(&dir, &info.addr);
    let payload = format!("{}\n", serde_json::to_string_pretty(info)?);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, &payload).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("publishing {}", path.display()))?;
    Ok(path)
}

/// Read a single registry entry by addr, if present and parseable.
#[must_use]
pub fn read_entry(addr: &str) -> Option<HttpServerInfo> {
    let path = registry_file(&registry_dir(), addr);
    read_entry_file(&path)
}

fn read_entry_file(path: &Path) -> Option<HttpServerInfo> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<HttpServerInfo>(raw.trim()).ok()
}

/// List EVERY registry entry (live or stale) without pruning. Sorted by addr for
/// deterministic output.
#[must_use]
pub fn list_entries() -> Vec<HttpServerInfo> {
    let dir = registry_dir();
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<HttpServerInfo> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|p| read_entry_file(&p))
        .collect();
    out.sort_by(|a, b| a.addr.cmp(&b.addr));
    out
}

/// Prune every registry entry whose pid is no longer alive (self-heal). Returns
/// the addrs that were pruned. A live entry is never touched.
pub fn prune_dead() -> Vec<String> {
    let dir = registry_dir();
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut pruned = Vec::new();
    for path in read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
    {
        match read_entry_file(&path) {
            Some(info) if is_process_alive(info.pid) => {}
            Some(info) => {
                if fs::remove_file(&path).is_ok() {
                    pruned.push(info.addr);
                }
            }
            None => {
                // Unparseable/corrupt file: remove it so it stops shadowing a
                // future healthy entry for the same addr.
                let _ = fs::remove_file(&path);
            }
        }
    }
    pruned.sort();
    pruned
}

/// List entries after pruning dead ones — the canonical "what is running now".
#[must_use]
pub fn live_entries() -> Vec<HttpServerInfo> {
    prune_dead();
    list_entries()
}

/// Return the LIVE entry bound to `addr`, pruning it first if its pid is dead.
/// `None` means no live server currently holds `addr`.
#[must_use]
pub fn live_entry_for(addr: &str) -> Option<HttpServerInfo> {
    let info = read_entry(addr)?;
    if is_process_alive(info.pid) {
        Some(info)
    } else {
        remove_entry(addr);
        None
    }
}

/// Remove the registry entry for `addr` (best-effort; a missing file is already
/// the desired end state). Returns true when a file was removed.
pub fn remove_entry(addr: &str) -> bool {
    let path = registry_file(&registry_dir(), addr);
    fs::remove_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    // The registry dir is process-global env state; serialize env-mutating tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempRegistry {
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl TempRegistry {
        fn new(label: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "cg-http-reg-{label}-{}-{}",
                std::process::id(),
                now_millis()
            ));
            fs::create_dir_all(&dir).unwrap();
            let prev = std::env::var_os(CODEGRAPH_HTTP_REGISTRY_DIR);
            // SAFETY: guarded by ENV_LOCK; single-threaded within the guard.
            unsafe { std::env::set_var(CODEGRAPH_HTTP_REGISTRY_DIR, &dir) };
            Self {
                dir,
                _guard: guard,
                prev,
            }
        }
    }

    impl Drop for TempRegistry {
        fn drop(&mut self) {
            // SAFETY: guarded by ENV_LOCK for the guard's lifetime.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(CODEGRAPH_HTTP_REGISTRY_DIR, v),
                    None => std::env::remove_var(CODEGRAPH_HTTP_REGISTRY_DIR),
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn sample(addr: &str, pid: u32, mode: HttpMode) -> HttpServerInfo {
        HttpServerInfo {
            pid,
            addr: addr.to_string(),
            mode,
            project: None,
            started_at: 1_700_000_000_000,
            version: "0.0.0-test".to_string(),
            log_file: Some("/tmp/x.log".to_string()),
        }
    }

    #[test]
    fn sanitize_addr_replaces_colon_and_slash() {
        assert_eq!(sanitize_addr("0.0.0.0:12025"), "0.0.0.0_12025");
        assert_eq!(sanitize_addr("127.0.0.1:8111"), "127.0.0.1_8111");
        assert_eq!(sanitize_addr("[::1]:8111"), "[__1]_8111");
        assert_eq!(sanitize_addr("a/b\\c:1"), "a_b_c_1");
    }

    #[test]
    fn registry_file_uses_sanitized_stem_and_json_ext() {
        let dir = Path::new("/state/http");
        assert_eq!(
            registry_file(dir, "0.0.0.0:12025"),
            Path::new("/state/http/0.0.0.0_12025.json")
        );
    }

    #[test]
    fn registry_dir_honors_explicit_override() {
        let _reg = TempRegistry::new("dir-override");
        let dir = registry_dir();
        assert!(
            dir.ends_with(_reg.dir.file_name().unwrap()),
            "explicit CODEGRAPH_HTTP_REGISTRY_DIR must win: {}",
            dir.display()
        );
    }

    #[test]
    fn http_server_info_roundtrips_through_json() {
        let info = HttpServerInfo {
            pid: 4242,
            addr: "127.0.0.1:19001".to_string(),
            mode: HttpMode::Pinned,
            project: Some("/home/u/proj".to_string()),
            started_at: 1_700_000_000_123,
            version: "1.2.3".to_string(),
            log_file: Some("/state/http/127.0.0.1_19001.log".to_string()),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(
            json.contains("\"mode\":\"pinned\""),
            "mode lowercases: {json}"
        );
        let back: HttpServerInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);
    }

    #[test]
    fn global_mode_serializes_lowercase() {
        let info = sample("0.0.0.0:9", 1, HttpMode::Global);
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"mode\":\"global\""), "{json}");
    }

    #[test]
    fn write_then_read_roundtrips() {
        let _reg = TempRegistry::new("write-read");
        let info = sample("127.0.0.1:19001", std::process::id(), HttpMode::Pinned);
        write_entry(&info).unwrap();
        let back = read_entry("127.0.0.1:19001").expect("entry present after write");
        assert_eq!(back, info);
    }

    #[test]
    fn prune_removes_dead_pid_entry_keeps_live() {
        let _reg = TempRegistry::new("prune");
        // Live: our own pid.
        let live = sample("127.0.0.1:19001", std::process::id(), HttpMode::Pinned);
        write_entry(&live).unwrap();
        // Dead: pid 0 is never alive on unix/windows liveness checks. Use a very
        // high unlikely-to-exist pid too for belt-and-suspenders.
        let dead = sample("127.0.0.1:19002", pick_dead_pid(), HttpMode::Global);
        write_entry(&dead).unwrap();

        let pruned = prune_dead();
        assert!(
            pruned.contains(&"127.0.0.1:19002".to_string()),
            "dead entry must be pruned: {pruned:?}"
        );
        assert!(
            read_entry("127.0.0.1:19002").is_none(),
            "dead entry file must be gone"
        );
        assert!(
            read_entry("127.0.0.1:19001").is_some(),
            "live entry must survive prune"
        );
    }

    #[test]
    fn live_entry_for_prunes_dead_and_returns_none() {
        let _reg = TempRegistry::new("live-for");
        let dead = sample("127.0.0.1:19003", pick_dead_pid(), HttpMode::Global);
        write_entry(&dead).unwrap();
        assert!(
            live_entry_for("127.0.0.1:19003").is_none(),
            "a dead entry must not be reported as a live conflict"
        );
        assert!(
            read_entry("127.0.0.1:19003").is_none(),
            "live_entry_for must have pruned the dead entry"
        );
    }

    #[test]
    fn live_entry_for_returns_live() {
        let _reg = TempRegistry::new("live-yes");
        let live = sample("127.0.0.1:19004", std::process::id(), HttpMode::Pinned);
        write_entry(&live).unwrap();
        let got = live_entry_for("127.0.0.1:19004").expect("live entry reported");
        assert_eq!(got.pid, std::process::id());
    }

    #[test]
    fn list_entries_sorted_by_addr() {
        let _reg = TempRegistry::new("list-sorted");
        write_entry(&sample(
            "127.0.0.1:19010",
            std::process::id(),
            HttpMode::Global,
        ))
        .unwrap();
        write_entry(&sample(
            "127.0.0.1:19002",
            std::process::id(),
            HttpMode::Global,
        ))
        .unwrap();
        let addrs: Vec<String> = list_entries().into_iter().map(|i| i.addr).collect();
        assert_eq!(addrs, vec!["127.0.0.1:19002", "127.0.0.1:19010"]);
    }

    #[test]
    fn remove_entry_deletes_file() {
        let _reg = TempRegistry::new("remove");
        write_entry(&sample(
            "127.0.0.1:19005",
            std::process::id(),
            HttpMode::Pinned,
        ))
        .unwrap();
        assert!(remove_entry("127.0.0.1:19005"));
        assert!(read_entry("127.0.0.1:19005").is_none());
    }

    /// A pid that is (almost certainly) not alive: try a very high pid; if by
    /// cosmic chance it is alive, fall back to 0 which liveness always rejects.
    fn pick_dead_pid() -> u32 {
        let candidate = 4_000_000_000u32;
        if is_process_alive(candidate) {
            0
        } else {
            candidate
        }
    }

    #[test]
    fn http_mode_as_str_matches_lowercase_variants() {
        assert_eq!(HttpMode::Pinned.as_str(), "pinned");
        assert_eq!(HttpMode::Global.as_str(), "global");
    }

    #[test]
    fn now_millis_is_nonzero_and_monotonic_enough() {
        let a = now_millis();
        assert!(a > 0);
        let b = now_millis();
        assert!(b >= a);
    }

    #[test]
    fn read_entry_absent_addr_returns_none() {
        let _reg = TempRegistry::new("read-absent");
        assert!(read_entry("127.0.0.1:65000").is_none());
    }

    #[test]
    fn live_entries_prunes_dead_and_returns_only_live() {
        let _reg = TempRegistry::new("live-entries");
        write_entry(&sample(
            "127.0.0.1:19020",
            std::process::id(),
            HttpMode::Pinned,
        ))
        .unwrap();
        write_entry(&sample(
            "127.0.0.1:19021",
            pick_dead_pid(),
            HttpMode::Global,
        ))
        .unwrap();
        let live = live_entries();
        let addrs: Vec<String> = live.into_iter().map(|i| i.addr).collect();
        assert_eq!(addrs, vec!["127.0.0.1:19020".to_string()]);
    }

    #[test]
    fn prune_dead_removes_unparseable_files() {
        let reg = TempRegistry::new("prune-corrupt");
        let corrupt = reg.dir.join("garbage.json");
        fs::write(&corrupt, b"{ not valid json").unwrap();
        prune_dead();
        assert!(!corrupt.exists(), "corrupt registry file must be removed");
    }

    #[test]
    fn list_entries_empty_when_registry_dir_missing() {
        let _reg = TempRegistry::new("list-empty");
        let _ = fs::remove_dir_all(&_reg.dir);
        assert!(list_entries().is_empty());
        assert!(prune_dead().is_empty());
    }

    #[test]
    fn remove_entry_missing_returns_false() {
        let _reg = TempRegistry::new("remove-missing");
        assert!(!remove_entry("127.0.0.1:65001"));
    }

    #[test]
    fn ensure_registry_dir_creates_the_directory() {
        let _reg = TempRegistry::new("ensure-dir");
        let _ = fs::remove_dir_all(&_reg.dir);
        let dir = ensure_registry_dir().unwrap();
        assert!(dir.exists());
    }

    #[test]
    fn registry_dir_ignores_empty_override_env() {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(CODEGRAPH_HTTP_REGISTRY_DIR);
        // SAFETY: guarded by ENV_LOCK; single-threaded within the guard.
        unsafe { std::env::set_var(CODEGRAPH_HTTP_REGISTRY_DIR, "") };
        let dir = registry_dir();
        assert!(
            dir.ends_with("http"),
            "empty override falls back: {}",
            dir.display()
        );
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CODEGRAPH_HTTP_REGISTRY_DIR, v),
                None => std::env::remove_var(CODEGRAPH_HTTP_REGISTRY_DIR),
            }
        }
        drop(guard);
    }
}
