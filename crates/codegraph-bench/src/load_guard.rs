//! The load guard of `bench --ab` (OD-B87): timing starts on a quiet host, and
//! a run that shared its CPUs with other work is void and runs again.
//!
//! - At the start of a session, [`LoadGuard::start_checks`] checks in a row,
//!   [`LoadGuard::start_interval_secs`] apart, must each find the 1-minute load
//!   below [`LoadGuard::start_load1_max`], no build process of any user
//!   ([`is_build_process`]), and at least [`LoadGuard::start_idle_min`] of the
//!   CPU time idle over a [`LoadGuard::sample_secs`] read of `/proc/stat`. The
//!   guard gives up after [`LoadGuard::start_max_wait_secs`], and nothing is
//!   timed.
//! - Before each run, the 1-minute load must be below
//!   [`LoadGuard::run_load1_max`] and a sample must find the cell's idle CPUs
//!   ([`LoadGuard::idle_cpus_required`]). The guard waits for both,
//!   [`LoadGuard::run_max_wait_secs`] at most.
//! - After each run, the CPU time every CPU spent busy during the run, less the
//!   child's own (from `wait4`) and the harness's, divided by the run's wall
//!   time, is the foreign load. Over the cell's limit
//!   ([`LoadGuard::foreign_cpus_limit`]) the run is void; the harness runs it
//!   again, each binary of a cell at most
//!   [`LoadGuard::max_attempts_factor`] times its requested runs.
//!
//! A cell is saturating when its threads cover every CPU (the `default` cell
//! included): it needs 7/8 of the CPUs idle before a run and tolerates 0.5
//! foreign CPU; any other cell needs 3/4 idle and tolerates 2. On 32 CPUs that
//! is 28 idle CPUs for 32 threads and 24 for 4, and on a 4-CPU runner 3 for 4
//! threads.

// Only the Linux probe reads `/proc`.
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Aggregate CPU time from the `cpu` line of `/proc/stat`, in clock ticks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuTimes {
    /// `user + nice + system + irq + softirq + steal`; guest time is already
    /// inside `user` and `nice`.
    pub busy: u64,
    /// `idle + iowait`.
    pub idle: u64,
}

impl CpuTimes {
    /// Read the aggregate `cpu` line of a `/proc/stat` text.
    pub fn parse(stat: &str) -> Result<Self> {
        let line = stat
            .lines()
            .find(|line| line.split_whitespace().next() == Some("cpu"))
            .context("/proc/stat has no aggregate `cpu` line")?;
        let fields = line
            .split_whitespace()
            .skip(1)
            .map(|field| {
                field
                    .parse::<u64>()
                    .with_context(|| format!("/proc/stat field {field:?} is not a count"))
            })
            .collect::<Result<Vec<_>>>()?;
        if fields.len() < 8 {
            bail!(
                "/proc/stat `cpu` line has {} fields, expected at least 8",
                fields.len()
            );
        }
        let [user, nice, system, idle, iowait, irq, softirq, steal] =
            <[u64; 8]>::try_from(&fields[..8]).expect("eight fields");
        Ok(Self {
            busy: user + nice + system + irq + softirq + steal,
            idle: idle + iowait,
        })
    }
}

/// The fraction of CPU time idle between two readings; `None` when the
/// counters did not move.
pub fn idle_fraction(before: CpuTimes, after: CpuTimes) -> Option<f64> {
    let busy = after.busy.saturating_sub(before.busy);
    let idle = after.idle.saturating_sub(before.idle);
    let total = busy + idle;
    (total > 0).then(|| idle as f64 / total as f64)
}

/// The 1-minute load average of a `/proc/loadavg` text.
pub fn parse_load1(loadavg: &str) -> Result<f64> {
    loadavg
        .split_whitespace()
        .next()
        .context("/proc/loadavg is empty")?
        .parse()
        .context("/proc/loadavg does not start with a load average")
}

/// Whether a process name (`/proc/<pid>/comm`) is a build the guard waits
/// out: cargo and its subcommands, rustc, rustdoc and clippy.
pub fn is_build_process(name: &str) -> bool {
    matches!(name, "cargo" | "rustc" | "rustdoc" | "clippy-driver") || name.starts_with("cargo-")
}

/// What the guard reads and how it waits: `/proc` on Linux ([`ProcProbe`]),
/// a script in tests.
pub trait LoadProbe {
    /// The aggregate CPU times now.
    fn cpu_times(&mut self) -> Result<CpuTimes>;
    /// Clock ticks per second of [`Self::cpu_times`].
    fn ticks_per_second(&self) -> f64;
    /// The 1-minute load average now.
    fn load1(&mut self) -> Result<f64>;
    /// The names of the build processes running, of any user.
    fn build_processes(&mut self) -> Result<Vec<String>>;
    /// The CPU time the harness itself has used so far.
    fn own_cpu(&mut self) -> Duration;
    /// Wait this long.
    fn pause(&mut self, duration: Duration);
}

/// The host's `/proc`.
#[cfg(target_os = "linux")]
pub struct ProcProbe {
    root: PathBuf,
    ticks_per_second: f64,
}

#[cfg(target_os = "linux")]
impl ProcProbe {
    /// The probe of this host; fails when `/proc/stat` cannot be read.
    pub fn new() -> Result<Self> {
        let root = PathBuf::from("/proc");
        fs::read_to_string(root.join("stat")).context("the load guard reads /proc/stat")?;
        // SAFETY: sysconf only reads a configuration value.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks <= 0 {
            bail!("sysconf(_SC_CLK_TCK) gave {ticks}");
        }
        Ok(Self {
            root,
            ticks_per_second: ticks as f64,
        })
    }
}

#[cfg(target_os = "linux")]
impl LoadProbe for ProcProbe {
    fn cpu_times(&mut self) -> Result<CpuTimes> {
        CpuTimes::parse(&fs::read_to_string(self.root.join("stat")).context("reading /proc/stat")?)
    }

    fn ticks_per_second(&self) -> f64 {
        self.ticks_per_second
    }

    fn load1(&mut self) -> Result<f64> {
        parse_load1(
            &fs::read_to_string(self.root.join("loadavg")).context("reading /proc/loadavg")?,
        )
    }

    fn build_processes(&mut self) -> Result<Vec<String>> {
        let mut builds = Vec::new();
        for entry in fs::read_dir(&self.root).context("listing /proc")? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if !name
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            {
                continue;
            }
            // A process can exit between the listing and the read.
            let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
                continue;
            };
            let comm = comm.trim_end().to_string();
            if is_build_process(&comm) {
                builds.push(comm);
            }
        }
        builds.sort();
        Ok(builds)
    }

    fn own_cpu(&mut self) -> Duration {
        // SAFETY: `rusage` is a plain C struct of integers; all-zero is valid
        // and getrusage overwrites it.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: the out-pointer is valid for writes for the call.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return Duration::ZERO;
        }
        cpu_of(&usage)
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// User plus system time of a `rusage`.
#[cfg(unix)]
pub(crate) fn cpu_of(usage: &libc::rusage) -> Duration {
    let time = |value: libc::timeval| {
        Duration::from_secs(u64::try_from(value.tv_sec).unwrap_or(0))
            + Duration::from_micros(u64::try_from(value.tv_usec).unwrap_or(0))
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// The guard's limits, recorded in the report's settings.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoadGuard {
    /// The CPUs the host has, from which the idle and foreign limits follow.
    pub logical_cpus: usize,
    /// Consecutive quiet checks needed before the first run; 0 skips them.
    pub start_checks: usize,
    pub start_interval_secs: f64,
    /// A start check needs the 1-minute load below this.
    pub start_load1_max: f64,
    /// A start check needs at least this fraction of CPU time idle.
    pub start_idle_min: f64,
    pub start_max_wait_secs: f64,
    /// A run waits for the 1-minute load to drop below this.
    pub run_load1_max: f64,
    /// Idle CPUs needed before a run, as a fraction of the CPUs, for a
    /// saturating cell and for another cell.
    pub run_idle_fraction_saturating: f64,
    pub run_idle_fraction_partial: f64,
    pub run_max_wait_secs: f64,
    /// How long one idle sample reads `/proc/stat` over.
    pub sample_secs: f64,
    /// The average foreign CPUs a run may share its time with, for a
    /// saturating cell and for another cell.
    pub foreign_cpus_saturating: f64,
    pub foreign_cpus_partial: f64,
    /// A cell runs each binary at most this many times its requested runs.
    pub max_attempts_factor: usize,
}

impl LoadGuard {
    /// The limits of `docs/benchmark.md` for a host with `logical_cpus` CPUs.
    pub fn standard(logical_cpus: usize) -> Self {
        Self {
            logical_cpus,
            start_checks: 3,
            start_interval_secs: 20.0,
            start_load1_max: 3.0,
            start_idle_min: 0.90,
            start_max_wait_secs: 80.0 * 60.0,
            run_load1_max: 8.0,
            run_idle_fraction_saturating: 7.0 / 8.0,
            run_idle_fraction_partial: 3.0 / 4.0,
            run_max_wait_secs: 10.0 * 60.0,
            sample_secs: 1.0,
            foreign_cpus_saturating: 0.5,
            foreign_cpus_partial: 2.0,
            max_attempts_factor: 2,
        }
    }

    /// These limits with the operator's overrides applied; an override that
    /// is negative or not a number is refused.
    pub fn overridden(mut self, overrides: &GuardOverrides) -> Result<Self> {
        for (name, value) in [
            (
                "foreign CPUs of a saturating cell",
                overrides.foreign_cpus_saturating,
            ),
            (
                "foreign CPUs of another cell",
                overrides.foreign_cpus_partial,
            ),
            ("quiet-start load", overrides.start_load1_max),
        ] {
            if let Some(value) = value
                && !(value.is_finite() && value >= 0.0)
            {
                bail!("the load guard's {name} must be a non-negative number, not {value}");
            }
        }
        if let Some(value) = overrides.foreign_cpus_saturating {
            self.foreign_cpus_saturating = value;
        }
        if let Some(value) = overrides.foreign_cpus_partial {
            self.foreign_cpus_partial = value;
        }
        if let Some(value) = overrides.start_load1_max {
            self.start_load1_max = value;
        }
        Ok(self)
    }

    /// Whether a cell's threads cover every CPU.
    pub fn saturating(&self, threads: Option<usize>) -> bool {
        threads.is_none_or(|threads| threads >= self.logical_cpus)
    }

    /// The idle CPUs a run of the cell needs before it starts.
    pub fn idle_cpus_required(&self, saturating: bool) -> f64 {
        let fraction = if saturating {
            self.run_idle_fraction_saturating
        } else {
            self.run_idle_fraction_partial
        };
        (self.logical_cpus as f64 * fraction).floor()
    }

    /// The average foreign CPUs a run of the cell may share its time with.
    pub fn foreign_cpus_limit(&self, saturating: bool) -> f64 {
        if saturating {
            self.foreign_cpus_saturating
        } else {
            self.foreign_cpus_partial
        }
    }

    /// The idle CPUs over one sample.
    fn sample_idle_cpus(&self, probe: &mut dyn LoadProbe) -> Result<f64> {
        let before = probe.cpu_times()?;
        probe.pause(Duration::from_secs_f64(self.sample_secs));
        let after = probe.cpu_times()?;
        let fraction = idle_fraction(before, after)
            .context("the CPU counters of /proc/stat did not move over a sample")?;
        Ok(fraction * self.logical_cpus as f64)
    }

    /// Wait for the quiet start: [`Self::start_checks`] passing checks in a row.
    pub fn wait_for_quiet_start(&self, probe: &mut dyn LoadProbe) -> Result<SessionStart> {
        let started = Instant::now();
        let mut checks = Vec::new();
        let mut in_a_row = 0;
        while in_a_row < self.start_checks {
            let load1 = probe.load1()?;
            let builds = probe.build_processes()?;
            let idle_fraction = self.sample_idle_cpus(probe)? / self.logical_cpus as f64;
            let passed = load1 < self.start_load1_max
                && builds.is_empty()
                && idle_fraction >= self.start_idle_min;
            checks.push(StartCheck {
                load1,
                idle_fraction,
                builds,
                passed,
            });
            in_a_row = if passed { in_a_row + 1 } else { 0 };
            if in_a_row == self.start_checks {
                break;
            }
            if started.elapsed().as_secs_f64() > self.start_max_wait_secs {
                let last = checks.last().expect("one check ran");
                bail!(
                    "the host was not quiet for {} checks in a row within {:.0} s, so nothing \
                     was timed (last check: load {:.2}, {:.0}% idle, builds {:?}; a check needs \
                     load below {:.1}, {:.0}% idle and no build)",
                    self.start_checks,
                    self.start_max_wait_secs,
                    last.load1,
                    last.idle_fraction * 100.0,
                    last.builds,
                    self.start_load1_max,
                    self.start_idle_min * 100.0
                );
            }
            probe.pause(Duration::from_secs_f64(self.start_interval_secs));
        }
        Ok(SessionStart {
            checks,
            waited_secs: started.elapsed().as_secs_f64(),
        })
    }

    /// Wait until a run of the cell may start: what the host showed then.
    pub fn wait_before_run(&self, probe: &mut dyn LoadProbe, saturating: bool) -> Result<RunGate> {
        let started = Instant::now();
        let required = self.idle_cpus_required(saturating);
        loop {
            let load1 = probe.load1()?;
            let idle_cpus = self.sample_idle_cpus(probe)?;
            if load1 < self.run_load1_max && idle_cpus >= required {
                return Ok(RunGate {
                    load1,
                    idle_cpus,
                    waited: started.elapsed(),
                });
            }
            if started.elapsed().as_secs_f64() > self.run_max_wait_secs {
                bail!(
                    "the host stayed busy for {:.0} s before a run (load {load1:.2}, \
                     {idle_cpus:.1} idle CPUs; a run needs load below {:.1} and {required:.0} \
                     idle CPUs)",
                    self.run_max_wait_secs,
                    self.run_load1_max
                );
            }
        }
    }

    /// Open the CPU-time window of a run, just before its child starts.
    pub fn open_window(&self, probe: &mut dyn LoadProbe) -> Result<RunWindow> {
        Ok(RunWindow {
            cpu: probe.cpu_times()?,
            own: probe.own_cpu(),
        })
    }

    /// The average foreign CPUs of a run, its child having used `child_cpu`
    /// over `wall`; read just after the child is reaped.
    pub fn foreign_cpus(
        &self,
        probe: &mut dyn LoadProbe,
        window: RunWindow,
        child_cpu: Duration,
        wall: Duration,
    ) -> Result<f64> {
        let after = probe.cpu_times()?;
        let own = probe.own_cpu().saturating_sub(window.own);
        let busy = after.busy.saturating_sub(window.cpu.busy) as f64 / probe.ticks_per_second();
        Ok(foreign_cpus(busy, child_cpu, own, wall))
    }
}

/// The average foreign CPUs over `wall`: the busy CPU seconds less the
/// child's and the harness's, never below zero.
pub fn foreign_cpus(busy_secs: f64, child: Duration, own: Duration, wall: Duration) -> f64 {
    let foreign = busy_secs - child.as_secs_f64() - own.as_secs_f64();
    foreign.max(0.0) / wall.as_secs_f64().max(1e-6)
}

/// The limits an operator sets instead of [`LoadGuard::standard`]'s
/// (`--guard-max-foreign-saturating`, `--guard-max-foreign-partial`,
/// `--guard-start-max-load`); `None` keeps the default.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GuardOverrides {
    pub foreign_cpus_saturating: Option<f64>,
    pub foreign_cpus_partial: Option<f64>,
    pub start_load1_max: Option<f64>,
}

/// One check of the quiet start.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StartCheck {
    pub load1: f64,
    pub idle_fraction: f64,
    pub builds: Vec<String>,
    pub passed: bool,
}

/// The quiet start of a session: every check, and how long it took.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionStart {
    pub checks: Vec<StartCheck>,
    pub waited_secs: f64,
}

/// What the host showed when a run started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunGate {
    pub load1: f64,
    pub idle_cpus: f64,
    pub waited: Duration,
}

/// The counters at the start of a run.
#[derive(Clone, Copy, Debug)]
pub struct RunWindow {
    cpu: CpuTimes,
    own: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  4705 356 584 3699176 23060 0 277 0 0 0\n\
                        cpu0 1393 280 373 1838280 20280 0 120 0 0 0\n\
                        intr 2132 49 0 0\n";

    #[test]
    fn the_aggregate_cpu_line_splits_into_busy_and_idle() {
        let times = CpuTimes::parse(STAT).unwrap();
        assert_eq!(times.busy, 4705 + 356 + 584 + 277);
        assert_eq!(times.idle, 3_699_176 + 23_060);
        assert!(CpuTimes::parse("cpu0 1 2 3 4 5 6 7 8\n").is_err());
        assert!(CpuTimes::parse("cpu 1 2 3\n").is_err());
        assert!(CpuTimes::parse("cpu 1 2 x 4 5 6 7 8\n").is_err());
    }

    #[test]
    fn idle_fraction_is_the_idle_share_of_the_ticks_that_passed() {
        let before = CpuTimes {
            busy: 100,
            idle: 900,
        };
        let after = CpuTimes {
            busy: 125,
            idle: 1_000,
        };
        assert_eq!(idle_fraction(before, after), Some(0.8));
        assert_eq!(idle_fraction(before, before), None);
    }

    #[test]
    fn the_load_average_is_the_first_field() {
        assert_eq!(parse_load1("2.50 1.00 0.75 1/300 42\n").unwrap(), 2.5);
        assert!(parse_load1("").is_err());
        assert!(parse_load1("busy 1 2").is_err());
    }

    #[test]
    fn builds_are_cargo_rustc_and_their_kin() {
        for name in ["cargo", "rustc", "rustdoc", "clippy-driver", "cargo-clippy"] {
            assert!(is_build_process(name), "{name}");
        }
        for name in ["codegraph", "bash", "cargox", "rust-analyzer"] {
            assert!(!is_build_process(name), "{name}");
        }
    }

    /// The draft's figures: 28 and 24 idle CPUs of 32, 3 of a 4-CPU runner,
    /// and the foreign limits on the same split.
    #[test]
    fn the_limits_follow_whether_the_threads_cover_every_cpu() {
        let host = LoadGuard::standard(32);
        assert!(host.saturating(Some(32)) && host.saturating(None));
        assert!(!host.saturating(Some(4)));
        assert_eq!(host.idle_cpus_required(host.saturating(Some(32))), 28.0);
        assert_eq!(host.idle_cpus_required(host.saturating(Some(4))), 24.0);
        assert_eq!(host.foreign_cpus_limit(true), 0.5);
        assert_eq!(host.foreign_cpus_limit(false), 2.0);
        let runner = LoadGuard::standard(4);
        assert!(runner.saturating(Some(4)));
        assert_eq!(runner.idle_cpus_required(true), 3.0);
    }

    /// An override replaces its default and leaves the others; an invalid one
    /// is refused.
    #[test]
    fn overrides_replace_only_the_limits_they_name() {
        let standard = LoadGuard::standard(32);
        assert_eq!(
            standard
                .clone()
                .overridden(&GuardOverrides::default())
                .unwrap(),
            standard
        );
        let guard = standard
            .clone()
            .overridden(&GuardOverrides {
                foreign_cpus_partial: Some(3.5),
                start_load1_max: Some(6.0),
                ..GuardOverrides::default()
            })
            .unwrap();
        assert_eq!(guard.foreign_cpus_limit(false), 3.5);
        assert_eq!(guard.foreign_cpus_limit(true), 0.5);
        assert_eq!(guard.start_load1_max, 6.0);
        assert_eq!(guard.run_load1_max, standard.run_load1_max);
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let overrides = GuardOverrides {
                foreign_cpus_saturating: Some(bad),
                ..GuardOverrides::default()
            };
            assert!(standard.clone().overridden(&overrides).is_err(), "{bad}");
        }
    }

    #[test]
    fn foreign_load_is_the_busy_time_the_child_and_harness_do_not_explain() {
        let second = Duration::from_secs(1);
        // 4 busy CPU seconds over 2 s: the child used 2.5, the harness 0.5.
        let foreign = foreign_cpus(4.0, second * 5 / 2, second / 2, second * 2);
        assert!((foreign - 0.5).abs() < 1e-9, "{foreign}");
        // Accounting jitter that reads below the child's own time is zero.
        assert_eq!(foreign_cpus(1.0, second * 2, Duration::ZERO, second), 0.0);
    }

    /// A scripted probe: `/proc/stat` readings from a queue, load and builds
    /// fixed per call.
    struct Script {
        stats: Vec<CpuTimes>,
        loads: Vec<f64>,
        builds: Vec<Vec<String>>,
        paused: Duration,
    }

    impl LoadProbe for Script {
        fn cpu_times(&mut self) -> Result<CpuTimes> {
            Ok(self.stats.remove(0))
        }
        fn ticks_per_second(&self) -> f64 {
            100.0
        }
        fn load1(&mut self) -> Result<f64> {
            Ok(self.loads.remove(0))
        }
        fn build_processes(&mut self) -> Result<Vec<String>> {
            Ok(self.builds.remove(0))
        }
        fn own_cpu(&mut self) -> Duration {
            Duration::ZERO
        }
        fn pause(&mut self, duration: Duration) {
            self.paused += duration;
        }
    }

    fn sample(idle_share: u64) -> [CpuTimes; 2] {
        [
            CpuTimes { busy: 0, idle: 0 },
            CpuTimes {
                busy: 100 - idle_share,
                idle: idle_share,
            },
        ]
    }

    /// A failing check restarts the count: a build in the second check needs
    /// three more quiet ones.
    #[test]
    fn the_quiet_start_needs_its_checks_in_a_row() {
        let guard = LoadGuard::standard(32);
        let mut script = Script {
            stats: [95, 95, 95, 95, 95].into_iter().flat_map(sample).collect(),
            loads: vec![0.5, 0.5, 0.5, 0.5, 0.5],
            builds: vec![vec![], vec!["rustc".to_string()], vec![], vec![], vec![]],
            paused: Duration::ZERO,
        };
        let start = guard.wait_for_quiet_start(&mut script).unwrap();
        let passed: Vec<bool> = start.checks.iter().map(|check| check.passed).collect();
        assert_eq!(passed, [true, false, true, true, true]);
        // Five 1 s samples and four 20 s intervals.
        assert_eq!(script.paused, Duration::from_secs(5 + 4 * 20));
    }

    #[test]
    fn a_busy_start_gives_up_after_its_wait() {
        let guard = LoadGuard {
            start_max_wait_secs: 0.0,
            ..LoadGuard::standard(32)
        };
        let mut script = Script {
            stats: sample(50).to_vec(),
            loads: vec![0.5],
            builds: vec![vec![]],
            paused: Duration::ZERO,
        };
        let error = guard.wait_for_quiet_start(&mut script).unwrap_err();
        assert!(
            format!("{error:#}").contains("nothing was timed"),
            "{error:#}"
        );
    }

    #[test]
    fn a_run_waits_for_its_idle_cpus_and_load() {
        let guard = LoadGuard::standard(32);
        // 28 of 32 CPUs idle is enough for a 32-thread cell; 27 is not, and
        // neither is a load of 8.
        let mut script = Script {
            stats: [84, 88, 88].into_iter().flat_map(sample).collect(),
            loads: vec![1.0, 8.0, 7.9],
            builds: Vec::new(),
            paused: Duration::ZERO,
        };
        let gate = guard.wait_before_run(&mut script, true).unwrap();
        assert_eq!(gate.load1, 7.9);
        assert_eq!(gate.idle_cpus, 28.16);
        assert_eq!(script.paused, Duration::from_secs(3));
    }
}
