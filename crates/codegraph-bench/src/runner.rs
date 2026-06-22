use std::fs::{File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::metrics::{run_with_proc_status, stats};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    Warm,
    Cold,
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub command: String,
    pub runs: usize,
    pub discard_first: bool,
    pub mode: CacheMode,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunSummary {
    pub command: String,
    pub mode: CacheMode,
    pub runs_ms: Vec<f64>,
    pub peak_rss_kb: Vec<Option<u64>>,
    pub median_ms: f64,
    pub mad_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub discarded_first: bool,
    pub raw_runs: usize,
    pub cold_supported: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            runs: 12,
            discard_first: true,
            mode: CacheMode::Warm,
        }
    }
}

pub fn run_command(config: &RunConfig, cwd: Option<&Path>) -> Result<RunSummary> {
    if config.runs == 0 {
        bail!("runs must be greater than zero");
    }

    let cold_supported = cold_mode_supported();
    let mut raw_ms = Vec::with_capacity(config.runs);
    let mut raw_rss = Vec::with_capacity(config.runs);

    for _ in 0..config.runs {
        if config.mode == CacheMode::Cold {
            if cold_supported {
                drop_caches()?;
            } else if let Some(dir) = cwd {
                evict_path_from_cache(dir);
            }
        }
        let measurement = run_with_proc_status(&config.command, cwd)?;
        if measurement.status_code != Some(0) {
            bail!(
                "command exited with {:?}\nstdout:\n{}\nstderr:\n{}",
                measurement.status_code,
                measurement.stdout,
                measurement.stderr
            );
        }
        raw_ms.push(measurement.duration_ms);
        raw_rss.push(measurement.peak_rss_kb);
    }

    let start = usize::from(config.discard_first && raw_ms.len() > 1);
    let runs_ms = raw_ms[start..].to_vec();
    let peak_rss_kb = raw_rss[start..].to_vec();
    let latency = stats(&runs_ms).context("cannot compute latency stats")?;

    Ok(RunSummary {
        command: config.command.clone(),
        mode: config.mode,
        runs_ms,
        peak_rss_kb,
        median_ms: latency.median,
        mad_ms: latency.mad,
        p50_ms: latency.p50,
        p99_ms: latency.p99,
        discarded_first: start == 1,
        raw_runs: config.runs,
        cold_supported,
    })
}

pub fn cold_mode_supported() -> bool {
    OpenOptions::new()
        .write(true)
        .open("/proc/sys/vm/drop_caches")
        .is_ok()
}

/// Best-effort userspace page-cache eviction for when `/proc/sys/vm/drop_caches`
/// is not writable (unprivileged container). Recursively walks `dir`, syncs each
/// regular file to flush dirty pages, then advises the kernel to drop its clean
/// pages via `posix_fadvise(POSIX_FADV_DONTNEED)`. No root required. Per-file
/// errors are ignored — this only sharpens "cold" measurements, never correctness.
pub fn evict_path_from_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            evict_path_from_cache(&path);
        } else if let Ok(file) = File::open(&path) {
            let _ = file.sync_all();
            // SAFETY: `posix_fadvise` only reads kernel page-cache state for the
            // given valid borrowed fd; it mutates no Rust memory and the fd
            // outlives the call. A non-zero return is advisory and ignored.
            #[cfg(unix)]
            unsafe {
                libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
            }
            // On non-Unix targets there is no `posix_fadvise`; eviction is a
            // best-effort Unix optimization, so this arm is a no-op (the file is
            // still synced above). The bind silences the unused-variable warning.
            #[cfg(not(unix))]
            let _ = &file;
        }
    }
}

fn drop_caches() -> Result<()> {
    let status = Command::new("sync").status().context("running sync")?;
    if !status.success() {
        bail!("sync failed with {status}");
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open("/proc/sys/vm/drop_caches")
        .context("opening /proc/sys/vm/drop_caches")?;
    file.write_all(b"3\n")
        .context("writing /proc/sys/vm/drop_caches")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::evict_path_from_cache;

    #[test]
    fn evict_path_from_cache_runs_on_temp_dir() {
        let dir = std::env::temp_dir().join(format!("cg-evict-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(sub.join("b.txt"), b"world").unwrap();

        evict_path_from_cache(&dir);

        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(sub.join("b.txt")).unwrap(), b"world");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
