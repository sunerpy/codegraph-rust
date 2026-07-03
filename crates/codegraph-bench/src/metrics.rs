use std::fs;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct ProcessMeasurement {
    pub duration_ms: f64,
    pub peak_rss_kb: Option<u64>,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LatencyStats {
    pub median: f64,
    pub mad: f64,
    pub p50: f64,
    pub p99: f64,
}

pub fn time_v_available() -> bool {
    Path::new("/usr/bin/time").exists()
}

pub fn run_with_proc_status(command: &str, cwd: Option<&Path>) -> Result<ProcessMeasurement> {
    run_with_proc_status_timeout(command, cwd, None)
}

/// Like [`run_with_proc_status`] but kills the child process group if it does
/// not exit within `timeout`. The upstream CLI on Node 25 intermittently fails
/// to exit after indexing (it stalls in `ep_poll` with the index already
/// written); without a wall-clock cap a single stalled run would hang the whole
/// benchmark. A timeout kill is surfaced as a non-zero status so the run fails
/// loudly rather than silently producing a bogus timing. The child is started
/// in its own process group so the kill reaches the upstream `--liftoff-only`
/// Node subchild.
pub fn run_with_proc_status_timeout(
    command: &str,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<ProcessMeasurement> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(format!("exec {command}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Unix-only: put the child in its own process group so the timeout kill
    // reaches the whole tree (the upstream `--liftoff-only` Node subchild).
    // `process_group` is a Unix `CommandExt` method; on Windows we spawn
    // normally and clean up via `child.kill()` below.
    #[cfg(unix)]
    process.process_group(0);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }

    let started = Instant::now();
    let mut child = process
        .spawn()
        .with_context(|| format!("running command with /proc RSS polling: {command}"))?;
    let pid = child.id();

    // The upstream `--liftoff-only` Node grandchild inherits and keeps the stdout
    // pipe open after the parent exits, so a blocking `wait_with_output()` would
    // hang forever draining a pipe nobody is writing to. Drain both streams on
    // dedicated threads instead; the main loop only waits on process exit and
    // can fire the wall-clock timeout independently of pipe state.
    let stdout_handle = child.stdout.take().map(spawn_drain);
    let stderr_handle = child.stderr.take().map(spawn_drain);

    let mut peak_rss_kb = None;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        peak_rss_kb = peak_rss_kb.max(read_proc_status_kb(pid, "VmHWM:"));
        if let Some(limit) = timeout
            && started.elapsed() > limit
        {
            // Unix kills the whole process group (negative PID); Windows has
            // no POSIX process group, so kill just the spawned child.
            #[cfg(unix)]
            kill_process_group(pid);
            #[cfg(not(unix))]
            let _ = child.kill();
            timed_out = true;
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(2));
    };
    peak_rss_kb = peak_rss_kb.max(read_proc_status_kb(pid, "VmHWM:"));
    let duration = started.elapsed();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let mut stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let status_code = if timed_out {
        stderr.push_str(&format!(
            "\n[codegraph-bench] killed after exceeding {:?} wall-clock timeout",
            timeout
        ));
        Some(124)
    } else {
        status.and_then(|s| s.code())
    };

    Ok(ProcessMeasurement {
        duration_ms: duration_to_ms(duration),
        peak_rss_kb,
        status_code,
        stdout,
        stderr,
    })
}

fn spawn_drain<R>(mut reader: R) -> thread::JoinHandle<String>
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).to_string()
    })
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // Negative PID targets the whole process group (created via process_group(0)).
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pid}"))
        .status();
}

pub fn run_with_time_v(command: &str, cwd: Option<&Path>) -> Result<ProcessMeasurement> {
    let mut process = Command::new("/usr/bin/time");
    process.args(["-v", "sh", "-c", command]);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }

    let started = Instant::now();
    let output = process
        .output()
        .with_context(|| format!("running smoke command via /usr/bin/time: {command}"))?;
    let duration = started.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(ProcessMeasurement {
        duration_ms: duration_to_ms(duration),
        peak_rss_kb: parse_time_v_peak_rss_kb(&stderr),
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr,
    })
}

pub fn db_file_size_bytes(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("stat db file {}", path.display()))?
        .len())
}

pub fn parse_time_v_peak_rss_kb(stderr: &str) -> Option<u64> {
    stderr.lines().find_map(|line| {
        let (_, value) = line.split_once("Maximum resident set size (kbytes):")?;
        value.trim().parse().ok()
    })
}

pub fn stats(samples: &[f64]) -> Option<LatencyStats> {
    Some(LatencyStats {
        median: median(samples)?,
        mad: mad(samples)?,
        p50: percentile(samples, 50.0)?,
        p99: percentile(samples, 99.0)?,
    })
}

pub fn median(samples: &[f64]) -> Option<f64> {
    percentile(samples, 50.0)
}

pub fn mad(samples: &[f64]) -> Option<f64> {
    let med = median(samples)?;
    let deviations: Vec<f64> = samples.iter().map(|sample| (sample - med).abs()).collect();
    median(&deviations)
}

pub fn percentile(samples: &[f64], percentile: f64) -> Option<f64> {
    if samples.is_empty() || !(0.0..=100.0).contains(&percentile) {
        return None;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() - 1) as f64 * percentile / 100.0;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        return Some(sorted[lower]);
    }
    let weight = rank - lower as f64;
    Some(sorted[lower] + (sorted[upper] - sorted[lower]) * weight)
}

fn duration_to_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn read_proc_status_kb(pid: u32, field: &str) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix(field)?.trim();
        value.split_whitespace().next()?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_odd_and_even_inputs() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn mad_uses_median_absolute_deviation() {
        let samples = [1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0];
        assert_eq!(median(&samples), Some(2.0));
        assert_eq!(mad(&samples), Some(1.0));
    }

    #[test]
    fn percentile_interpolates_between_neighbors() {
        let samples = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(percentile(&samples, 0.0), Some(10.0));
        assert_eq!(percentile(&samples, 50.0), Some(25.0));
        assert_eq!(percentile(&samples, 99.0), Some(39.7));
        assert_eq!(percentile(&samples, 100.0), Some(40.0));
        assert_eq!(percentile(&samples, 101.0), None);
    }

    #[test]
    fn parses_gnu_time_verbose_peak_rss() {
        let stderr = "\tMaximum resident set size (kbytes): 12345\n";
        assert_eq!(parse_time_v_peak_rss_kb(stderr), Some(12345));
    }
}
