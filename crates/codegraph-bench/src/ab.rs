//! `bench --ab`: cold-`init` A/B timing of two CodeGraph binaries.
//!
//! For every corpus, thread count and run, the harness copies the corpus
//! subtree into a fresh temporary project (without `.git` or an index
//! directory) and runs `<binary> init <project>` with
//! `RAYON_NUM_THREADS=<threads>`, `CODEGRAPH_NO_DAEMON=1` and
//! `CODEGRAPH_NO_WATCH=1`, and with `CODEGRAPH_DIR` removed. The CLI has no
//! thread flag; parsing and resolution run on rayon's global pool, which reads
//! `RAYON_NUM_THREADS`. Each run records its wall time, its peak RSS (`wait4`
//! `ru_maxrss` on Unix) and the canonical fingerprint of the database it wrote.
//!
//! Baseline and candidate runs alternate (even runs baseline first, odd runs
//! candidate first). The first run of every (corpus, binary, threads) cell is a
//! warm-up and is left out of the statistics.
//!
//! The harness fails closed and writes no report when a run exits non-zero, is
//! killed or times out; when a run exits 0 without a database, or with one that
//! fails `PRAGMA quick_check` or canonicalization; and when two runs of the
//! same binary on one corpus, at any thread counts, produce different canonical
//! graphs. A baseline/candidate graph difference is not an error: it is
//! reported per corpus, so a candidate that changes the graph can still be
//! timed against its row growth.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use codegraph_core::index_paths::{DEFAULT_CURRENT_DIR, WSL_CURRENT_DIR};
use serde::{Deserialize, Serialize};

use crate::corpus::{Corpus, benchmark_path_in, checkout_path_in};
use crate::graph_diff::{
    GraphFingerprint, ScratchDir, canonicalize_checked, differing_surfaces, fingerprint,
    sha256_file,
};
use crate::metrics::{mad, median};
use crate::report::{command_stdout, read_cpu_model, read_mem_total_kb, read_os_pretty_name};

/// Environment variable that sets the thread count of each timed run.
pub const THREAD_ENV: &str = "RAYON_NUM_THREADS";
/// Default wall-clock cap for one timed run; a run past it is killed and fails.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Regression budget (`docs/benchmark.md`): candidate/baseline cold-`init`
/// median wall time.
pub const INIT_WALL_RATIO_MAX: f64 = 1.10;
/// A slower `init` is still within budget when the candidate graph has more
/// canonical rows and its rows/s stay at or above this ratio.
pub const ROWS_PER_SECOND_RATIO_MIN: f64 = 0.90;
/// Regression budget: candidate/baseline median peak RSS.
pub const PEAK_RSS_RATIO_MAX: f64 = 1.15;

const REPORT_KIND: &str = "codegraph-ab";
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const STDERR_TAIL_LINES: usize = 40;
const DATABASE_FILE: &str = "codegraph.db";

/// What one `bench --ab` invocation compares.
#[derive(Clone, Debug)]
pub struct AbConfig {
    pub baseline: PathBuf,
    pub candidate: PathBuf,
    pub corpora: Vec<AbCorpus>,
    pub threads: Vec<usize>,
    /// Runs per (corpus, binary, threads) cell, including the warm-up.
    pub runs: usize,
    pub run_timeout: Duration,
    /// Workspace whose commit is recorded as the harness commit.
    pub workspace_root: Option<PathBuf>,
    /// The command line, recorded verbatim in the report.
    pub argv: Vec<String>,
}

/// One corpus to time: the subtree copied into every run's project.
#[derive(Clone, Debug)]
pub struct AbCorpus {
    pub name: String,
    pub commit: Option<String>,
    pub tag: Option<String>,
    pub source: PathBuf,
}

impl AbCorpus {
    /// Resolve a registry corpus under `corpora_root`. A checkout that is
    /// missing, at another commit than the pin, or modified is refused.
    pub fn from_registry(corpus: Corpus, corpora_root: &Path) -> Result<Self> {
        let checkout = checkout_path_in(corpora_root, corpus);
        if !checkout.is_dir() {
            bail!(
                "corpus {} is not fetched at {} (run `bench --fetch-corpora --corpora-root {}`)",
                corpus.name,
                checkout.display(),
                corpora_root.display()
            );
        }
        let head = git_stdout(&checkout, &["rev-parse", "HEAD"])
            .with_context(|| format!("reading the commit of {}", checkout.display()))?;
        if head != corpus.commit {
            bail!(
                "corpus {} at {} is at {head}, but the registry pins {}",
                corpus.name,
                checkout.display(),
                corpus.commit
            );
        }
        let status = git_stdout(
            &checkout,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .with_context(|| format!("reading the status of {}", checkout.display()))?;
        if !status.is_empty() {
            bail!(
                "corpus {} at {} has local changes; restore the pinned checkout first:\n{status}",
                corpus.name,
                checkout.display()
            );
        }
        let source = benchmark_path_in(corpora_root, corpus);
        if !source.is_dir() {
            bail!(
                "corpus {} has no benchmark directory {}",
                corpus.name,
                source.display()
            );
        }
        Ok(Self {
            name: corpus.name.to_string(),
            commit: Some(corpus.commit.to_string()),
            tag: corpus.tag.map(str::to_string),
            source,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AbReport {
    pub schema_version: u32,
    pub kind: String,
    pub argv: Vec<String>,
    pub environment: AbEnvironment,
    pub settings: AbSettings,
    pub baseline: BinaryInfo,
    pub candidate: BinaryInfo,
    pub corpora: Vec<CorpusAb>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AbEnvironment {
    pub cpu_model: Option<String>,
    pub logical_cpus: Option<usize>,
    pub memory_total_kb: Option<u64>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub rustc_version: Option<String>,
    pub harness_commit: Option<String>,
    pub harness_dirty: Option<bool>,
    pub rss_collector: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AbSettings {
    pub invocation: String,
    pub env_set: BTreeMap<String, String>,
    pub env_removed: Vec<String>,
    pub thread_env: String,
    pub threads: Vec<usize>,
    pub runs: usize,
    pub discarded_warmup_runs: usize,
    pub order: String,
    pub run_timeout_secs: u64,
    pub budget: BudgetLimits,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct BudgetLimits {
    pub init_wall_ratio_max: f64,
    pub rows_per_second_ratio_min: f64,
    pub peak_rss_ratio_max: f64,
}

impl BudgetLimits {
    pub const PROJECT: Self = Self {
        init_wall_ratio_max: INIT_WALL_RATIO_MAX,
        rows_per_second_ratio_min: ROWS_PER_SECOND_RATIO_MIN,
        peak_rss_ratio_max: PEAK_RSS_RATIO_MAX,
    };
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BinaryInfo {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// First line of `<binary> --version` when it exits 0 within 30 s.
    pub version: Option<String>,
    /// The resolved path that is executed; `path` is its display form.
    #[serde(skip)]
    executable: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CorpusAb {
    pub name: String,
    pub commit: Option<String>,
    pub tag: Option<String>,
    pub source: String,
    pub copied_files: u64,
    pub skipped_symlinks: u64,
    pub graph: GraphComparison,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphComparison {
    pub identical: bool,
    pub differing_surfaces: Vec<String>,
    pub baseline: GraphFingerprint,
    pub candidate: GraphFingerprint,
}

/// One corpus at one thread count.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Cell {
    pub threads: usize,
    pub baseline: ArmSeries,
    pub candidate: ArmSeries,
    pub ratio: Ratios,
    pub budget: BudgetVerdict,
}

/// Every run of one binary in a cell, and the statistics of the measured ones.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArmSeries {
    pub runs: Vec<RunSample>,
    pub wall_ms: Summary,
    pub peak_rss_kb: Option<Summary>,
    pub rows_per_second: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunSample {
    /// 1-based run number within the cell.
    pub run: usize,
    pub warmup: bool,
    pub wall_ms: f64,
    pub peak_rss_kb: Option<u64>,
    pub canonical_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Summary {
    pub median: f64,
    pub mad: f64,
    pub samples: usize,
}

impl Summary {
    fn of(samples: &[f64]) -> Option<Self> {
        Some(Self {
            median: median(samples)?,
            mad: mad(samples)?,
            samples: samples.len(),
        })
    }
}

/// Candidate divided by baseline; `None` when either side is missing or zero.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Ratios {
    pub wall_ms: Option<f64>,
    pub peak_rss_kb: Option<f64>,
    pub canonical_rows: Option<f64>,
    pub rows_per_second: Option<f64>,
}

/// The regression budget applied to one cell; `None` when not measurable.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct BudgetVerdict {
    pub init_wall_ok: Option<bool>,
    pub peak_rss_ok: Option<bool>,
}

impl BudgetVerdict {
    pub fn evaluate(ratio: &Ratios, limits: &BudgetLimits) -> Self {
        let init_wall_ok = ratio.wall_ms.map(|wall| {
            let rows_grew = ratio.canonical_rows.is_some_and(|rows| rows > 1.0);
            let throughput_held = ratio
                .rows_per_second
                .is_some_and(|rate| rate >= limits.rows_per_second_ratio_min);
            wall <= limits.init_wall_ratio_max || (rows_grew && throughput_held)
        });
        let peak_rss_ok = ratio
            .peak_rss_kb
            .map(|rss| rss <= limits.peak_rss_ratio_max);
        Self {
            init_wall_ok,
            peak_rss_ok,
        }
    }
}

/// Time both binaries on every configured corpus.
pub fn run_ab(config: &AbConfig) -> Result<AbReport> {
    validate(config)?;
    let scratch = ScratchDir::new("ab")?;
    let baseline = BinaryInfo::inspect(&config.baseline, &scratch.path().join("baseline.version"))
        .context("inspecting the --baseline binary")?;
    let candidate =
        BinaryInfo::inspect(&config.candidate, &scratch.path().join("candidate.version"))
            .context("inspecting the --candidate binary")?;
    let binaries = [&baseline, &candidate];
    let mut corpora = Vec::with_capacity(config.corpora.len());
    for (index, corpus) in config.corpora.iter().enumerate() {
        eprintln!("==> {} ({})", corpus.name, corpus.source.display());
        corpora.push(measure_corpus(
            config,
            index,
            corpus,
            binaries,
            scratch.path(),
        )?);
    }
    Ok(AbReport {
        schema_version: 1,
        kind: REPORT_KIND.to_string(),
        argv: config.argv.clone(),
        environment: AbEnvironment::detect(config.workspace_root.as_deref()),
        settings: AbSettings::of(config),
        baseline,
        candidate,
        corpora,
    })
}

fn validate(config: &AbConfig) -> Result<()> {
    if config.runs < 2 {
        bail!(
            "--runs must be at least 2: the first run of every cell is a discarded warm-up (got {})",
            config.runs
        );
    }
    if config.threads.is_empty() {
        bail!("--ab needs at least one thread count (--threads <n[,m]>)");
    }
    if config.threads.contains(&0) {
        bail!("thread counts must be at least 1");
    }
    let distinct: BTreeSet<_> = config.threads.iter().collect();
    if distinct.len() != config.threads.len() {
        bail!("thread counts must be distinct: {:?}", config.threads);
    }
    if config.corpora.is_empty() {
        bail!("--ab needs at least one corpus (--corpora <name[,name...]>)");
    }
    let names: BTreeSet<_> = config.corpora.iter().map(|corpus| &corpus.name).collect();
    if names.len() != config.corpora.len() {
        bail!("each corpus may be listed only once");
    }
    if config.run_timeout.is_zero() {
        bail!("the run timeout must be positive");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Arm {
    Baseline,
    Candidate,
}

impl Arm {
    fn index(self) -> usize {
        match self {
            Self::Baseline => 0,
            Self::Candidate => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }

    /// Even runs time the baseline first, odd runs the candidate first.
    fn order(run: usize) -> [Self; 2] {
        if run.is_multiple_of(2) {
            [Self::Baseline, Self::Candidate]
        } else {
            [Self::Candidate, Self::Baseline]
        }
    }
}

/// Names one timed run in progress lines and errors.
struct RunLabel<'a> {
    arm: Arm,
    corpus: &'a str,
    threads: usize,
    run: usize,
    runs: usize,
}

impl fmt::Display for RunLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} run {}/{} on {} ({}={})",
            self.arm.name(),
            self.run + 1,
            self.runs,
            self.corpus,
            THREAD_ENV,
            self.threads
        )
    }
}

fn measure_corpus(
    config: &AbConfig,
    index: usize,
    corpus: &AbCorpus,
    binaries: [&BinaryInfo; 2],
    scratch: &Path,
) -> Result<CorpusAb> {
    let mut graphs: [Option<GraphFingerprint>; 2] = [None, None];
    let mut copied = CopyStats::default();
    let mut cells = Vec::with_capacity(config.threads.len());
    for &threads in &config.threads {
        let mut series: [Vec<RunSample>; 2] = [Vec::new(), Vec::new()];
        for run in 0..config.runs {
            for arm in Arm::order(run) {
                let label = RunLabel {
                    arm,
                    corpus: &corpus.name,
                    threads,
                    run,
                    runs: config.runs,
                };
                let run_dir = scratch.join(format!("c{index}-{}-t{threads}-r{run}", arm.name()));
                let outcome = measure_run(
                    binaries[arm.index()],
                    corpus,
                    threads,
                    &run_dir,
                    config.run_timeout,
                )
                .with_context(|| format!("{label} failed"))?;
                fs::remove_dir_all(&run_dir)
                    .with_context(|| format!("removing {}", run_dir.display()))?;
                check_same_graph(&mut graphs[arm.index()], &outcome.graph, &label)?;
                copied = outcome.copied;
                let wall_ms = outcome.wall.as_secs_f64() * 1000.0;
                eprintln!(
                    "    {label}: {wall_ms:.1} ms, peak RSS {}{}",
                    outcome
                        .peak_rss_kb
                        .map_or_else(|| "n/a".to_string(), |kb| format!("{kb} KiB")),
                    if run == 0 { " (warm-up)" } else { "" }
                );
                series[arm.index()].push(RunSample {
                    run: run + 1,
                    warmup: run == 0,
                    wall_ms,
                    peak_rss_kb: outcome.peak_rss_kb,
                    canonical_hash: outcome.graph.hash.clone(),
                });
            }
        }
        let [baseline_runs, candidate_runs] = series;
        let rows = |arm: Arm| graphs[arm.index()].as_ref().map(|graph| graph.rows.total);
        let baseline = ArmSeries::new(baseline_runs, rows(Arm::Baseline))?;
        let candidate = ArmSeries::new(candidate_runs, rows(Arm::Candidate))?;
        let ratio = Ratios::between(
            &baseline,
            &candidate,
            rows(Arm::Baseline),
            rows(Arm::Candidate),
        );
        cells.push(Cell {
            threads,
            budget: BudgetVerdict::evaluate(&ratio, &BudgetLimits::PROJECT),
            baseline,
            candidate,
            ratio,
        });
    }
    let [Some(baseline), Some(candidate)] = graphs else {
        bail!("corpus {} produced no measured runs", corpus.name);
    };
    let differing = differing_surfaces(&baseline, &candidate);
    eprintln!(
        "    graph: {}",
        if differing.is_empty() {
            "identical".to_string()
        } else {
            format!("baseline and candidate differ in {}", differing.join(", "))
        }
    );
    Ok(CorpusAb {
        name: corpus.name.clone(),
        commit: corpus.commit.clone(),
        tag: corpus.tag.clone(),
        source: corpus.source.display().to_string(),
        copied_files: copied.files,
        skipped_symlinks: copied.skipped_symlinks,
        graph: GraphComparison {
            identical: differing.is_empty(),
            differing_surfaces: differing,
            baseline,
            candidate,
        },
        cells,
    })
}

fn check_same_graph(
    first: &mut Option<GraphFingerprint>,
    graph: &GraphFingerprint,
    label: &RunLabel<'_>,
) -> Result<()> {
    match first {
        None => {
            *first = Some(graph.clone());
            Ok(())
        }
        Some(expected) if expected.hash == graph.hash => Ok(()),
        Some(expected) => bail!(
            "non-deterministic graph: {label} produced canonical hash {}, but the first {} run on \
             {} produced {} (differing surfaces: {}); every run of one binary must produce the \
             same canonical graph",
            graph.hash,
            label.arm.name(),
            label.corpus,
            expected.hash,
            differing_surfaces(expected, graph).join(", ")
        ),
    }
}

struct RunOutcome {
    wall: Duration,
    peak_rss_kb: Option<u64>,
    graph: GraphFingerprint,
    copied: CopyStats,
}

fn measure_run(
    binary: &BinaryInfo,
    corpus: &AbCorpus,
    threads: usize,
    run_dir: &Path,
    timeout: Duration,
) -> Result<RunOutcome> {
    let project = run_dir.join("project");
    fs::create_dir_all(run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
    let copied = copy_corpus(&corpus.source, &project)?;
    let stdout_path = run_dir.join("init.stdout");
    let stderr_path = run_dir.join("init.stderr");
    let stdout = File::create(&stdout_path)
        .with_context(|| format!("creating {}", stdout_path.display()))?;
    let stderr = File::create(&stderr_path)
        .with_context(|| format!("creating {}", stderr_path.display()))?;
    let mut command = Command::new(&binary.executable);
    command
        .arg("init")
        .arg(&project)
        .current_dir(run_dir)
        .env(THREAD_ENV, threads.to_string())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env_remove("CODEGRAPH_DIR")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    let measured = spawn_measured(&mut command, timeout)
        .with_context(|| format!("running {} init", binary.path))?;
    match measured.exit {
        Exit::Code(0) => {}
        Exit::Code(code) => bail!(
            "`{} init` exited with status {code}{}",
            binary.path,
            stderr_tail(&stderr_path)
        ),
        #[cfg(unix)]
        Exit::Signal(signal) => bail!(
            "`{} init` was killed by signal {signal}{}",
            binary.path,
            stderr_tail(&stderr_path)
        ),
        Exit::TimedOut => bail!(
            "`{} init` exceeded the {} s run timeout and was killed{}",
            binary.path,
            timeout.as_secs(),
            stderr_tail(&stderr_path)
        ),
        Exit::Unknown => bail!("`{} init` ended without an exit status", binary.path),
    }
    let db = project.join(DEFAULT_CURRENT_DIR).join(DATABASE_FILE);
    if !db.is_file() {
        bail!(
            "`{} init` exited 0 but produced no database at {}",
            binary.path,
            db.display()
        );
    }
    let canonical = canonicalize_checked(&db)
        .with_context(|| format!("the database {} cannot be canonicalized", db.display()))?;
    Ok(RunOutcome {
        wall: measured.wall,
        peak_rss_kb: measured.peak_rss_kb,
        graph: fingerprint(&canonical),
        copied,
    })
}

fn stderr_tail(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    format!(
        "\nstderr (last {} lines):\n{}",
        lines.len() - start,
        lines[start..].join("\n")
    )
}

impl ArmSeries {
    fn new(runs: Vec<RunSample>, rows_total: Option<u64>) -> Result<Self> {
        let measured: Vec<&RunSample> = runs.iter().filter(|run| !run.warmup).collect();
        let walls: Vec<f64> = measured.iter().map(|run| run.wall_ms).collect();
        let wall_ms = Summary::of(&walls).context("a cell has no measured runs")?;
        let rss: Vec<f64> = measured
            .iter()
            .filter_map(|run| run.peak_rss_kb)
            .map(|kb| kb as f64)
            .collect();
        let peak_rss_kb = (rss.len() == measured.len())
            .then(|| Summary::of(&rss))
            .flatten();
        let rows_per_second = rows_total
            .filter(|_| wall_ms.median > 0.0)
            .map(|rows| rows as f64 / (wall_ms.median / 1000.0));
        Ok(Self {
            runs,
            wall_ms,
            peak_rss_kb,
            rows_per_second,
        })
    }
}

impl Ratios {
    fn between(
        baseline: &ArmSeries,
        candidate: &ArmSeries,
        baseline_rows: Option<u64>,
        candidate_rows: Option<u64>,
    ) -> Self {
        Self {
            wall_ms: ratio(
                Some(candidate.wall_ms.median),
                Some(baseline.wall_ms.median),
            ),
            peak_rss_kb: ratio(
                candidate.peak_rss_kb.map(|rss| rss.median),
                baseline.peak_rss_kb.map(|rss| rss.median),
            ),
            canonical_rows: ratio(
                candidate_rows.map(|rows| rows as f64),
                baseline_rows.map(|rows| rows as f64),
            ),
            rows_per_second: ratio(candidate.rows_per_second, baseline.rows_per_second),
        }
    }
}

fn ratio(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
    let (numerator, denominator) = (numerator?, denominator?);
    (denominator > 0.0 && numerator.is_finite() && denominator.is_finite())
        .then(|| numerator / denominator)
}

impl BinaryInfo {
    /// Hash and describe a binary; `version_out` receives its `--version` output.
    fn inspect(path: &Path, version_out: &Path) -> Result<Self> {
        let resolved =
            fs::canonicalize(path).with_context(|| format!("cannot find {}", path.display()))?;
        let metadata =
            fs::metadata(&resolved).with_context(|| format!("reading {}", resolved.display()))?;
        if !metadata.is_file() {
            bail!("{} is not a file", resolved.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                bail!("{} is not executable", resolved.display());
            }
        }
        Ok(Self {
            path: resolved.display().to_string(),
            sha256: sha256_file(&resolved)?,
            size_bytes: metadata.len(),
            version: probe_version(&resolved, version_out),
            executable: resolved,
        })
    }
}

/// First line of `<binary> --version`, or `None` when it fails or hangs.
fn probe_version(binary: &Path, out_path: &Path) -> Option<String> {
    let out = File::create(out_path).ok()?;
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(Stdio::null());
    let measured = spawn_measured(&mut command, VERSION_TIMEOUT).ok()?;
    let text = fs::read_to_string(out_path).ok()?;
    if measured.exit != Exit::Code(0) {
        return None;
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Exit {
    Code(i32),
    #[cfg(unix)]
    Signal(i32),
    TimedOut,
    Unknown,
}

struct Measured {
    exit: Exit,
    wall: Duration,
    peak_rss_kb: Option<u64>,
}

/// Spawn `command`, wait for it under `timeout`, and measure it. The wall time
/// runs from just before the spawn until the poll that observes the exit.
fn spawn_measured(command: &mut Command, timeout: Duration) -> Result<Measured> {
    let started = Instant::now();
    let child = command.spawn().context("spawning the process")?;
    wait_measured(child, started, timeout)
}

#[cfg(unix)]
fn wait_measured(child: Child, started: Instant, timeout: Duration) -> Result<Measured> {
    let pid = libc::pid_t::try_from(child.id()).context("the child pid does not fit pid_t")?;
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is a plain C struct of integers; all-zero is a valid value
    // and `wait4` overwrites it when it reaps the child.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let mut killed = false;
    loop {
        // SAFETY: `pid` is this process's own child, which only this loop ever
        // reaps (the std `Child` handle is dropped without waiting), and both
        // out-pointers are valid for writes for the duration of the call.
        let reaped = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if reaped == pid {
            break;
        }
        if reaped == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("waiting for the child process");
        }
        if !killed && started.elapsed() > timeout {
            // SAFETY: the child has not been reaped yet, so its pid still names
            // it and cannot have been reused by another process.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            killed = true;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let wall = started.elapsed();
    drop(child);
    let exit = if killed {
        Exit::TimedOut
    } else if libc::WIFEXITED(status) {
        Exit::Code(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Exit::Signal(libc::WTERMSIG(status))
    } else {
        Exit::Unknown
    };
    Ok(Measured {
        exit,
        wall,
        peak_rss_kb: max_rss_kb(&usage),
    })
}

/// `ru_maxrss` is KiB on Linux and the BSDs, bytes on macOS.
#[cfg(unix)]
fn max_rss_kb(usage: &libc::rusage) -> Option<u64> {
    let raw = u64::try_from(usage.ru_maxrss).ok()?;
    let kb = if cfg!(target_os = "macos") {
        raw / 1024
    } else {
        raw
    };
    (kb > 0).then_some(kb)
}

#[cfg(not(unix))]
fn wait_measured(mut child: Child, started: Instant, timeout: Duration) -> Result<Measured> {
    let mut killed = false;
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for the child process")? {
            break status;
        }
        if !killed && started.elapsed() > timeout {
            let _ = child.kill();
            killed = true;
        }
        thread::sleep(POLL_INTERVAL);
    };
    let wall = started.elapsed();
    let exit = if killed {
        Exit::TimedOut
    } else {
        status.code().map_or(Exit::Unknown, Exit::Code)
    };
    Ok(Measured {
        exit,
        wall,
        peak_rss_kb: None,
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CopyStats {
    files: u64,
    skipped_symlinks: u64,
}

/// Copy a corpus subtree, leaving out VCS metadata and index directories and
/// skipping symlinks, so every run starts from the same plain source tree.
fn copy_corpus(source: &Path, destination: &Path) -> Result<CopyStats> {
    let mut stats = CopyStats::default();
    copy_dir(source, destination, &mut stats)?;
    Ok(stats)
}

fn copy_dir(source: &Path, destination: &Path, stats: &mut CopyStats) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("reading {}", source.display()))? {
        let entry = entry.with_context(|| format!("reading {}", source.display()))?;
        let name = entry.file_name();
        if is_excluded(&name) {
            continue;
        }
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = destination.join(&name);
        if file_type.is_symlink() {
            stats.skipped_symlinks += 1;
        } else if file_type.is_dir() {
            copy_dir(&from, &to, stats)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
            stats.files += 1;
        }
    }
    Ok(())
}

fn is_excluded(name: &OsStr) -> bool {
    name == ".git" || name == DEFAULT_CURRENT_DIR || name == WSL_CURRENT_DIR
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

impl AbEnvironment {
    fn detect(workspace_root: Option<&Path>) -> Self {
        let harness_commit =
            workspace_root.and_then(|root| git_stdout(root, &["rev-parse", "HEAD"]).ok());
        let harness_dirty = workspace_root.and_then(|root| {
            git_stdout(root, &["status", "--porcelain"])
                .ok()
                .map(|status| !status.is_empty())
        });
        Self {
            cpu_model: read_cpu_model(),
            logical_cpus: thread::available_parallelism().ok().map(usize::from),
            memory_total_kb: read_mem_total_kb(),
            os: read_os_pretty_name(),
            kernel: command_stdout("uname", &["-sr"]),
            rustc_version: command_stdout("rustc", &["--version"]),
            harness_commit,
            harness_dirty,
            rss_collector: rss_collector().to_string(),
        }
    }
}

fn rss_collector() -> &'static str {
    if cfg!(target_os = "macos") {
        "wait4 rusage ru_maxrss (bytes, reported in KiB)"
    } else if cfg!(unix) {
        "wait4 rusage ru_maxrss (KiB)"
    } else {
        "unavailable"
    }
}

impl AbSettings {
    fn of(config: &AbConfig) -> Self {
        let env_set = [
            ("CODEGRAPH_NO_DAEMON", "1"),
            ("CODEGRAPH_NO_WATCH", "1"),
            (THREAD_ENV, "<threads>"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        Self {
            invocation: "<binary> init <fresh copy of the corpus>".to_string(),
            env_set,
            env_removed: vec!["CODEGRAPH_DIR".to_string()],
            thread_env: THREAD_ENV.to_string(),
            threads: config.threads.clone(),
            runs: config.runs,
            discarded_warmup_runs: 1,
            order: "interleaved: even runs baseline first, odd runs candidate first".to_string(),
            run_timeout_secs: config.run_timeout.as_secs(),
            budget: BudgetLimits::PROJECT,
        }
    }
}

/// Write the JSON report, creating parent directories.
pub fn write_ab_report(path: &Path, report: &AbReport) -> Result<()> {
    write_text(
        path,
        &serde_json::to_string_pretty(report).context("serializing the A/B report")?,
    )
}

/// Write the Markdown table rendered by [`render_ab_markdown`].
pub fn write_ab_markdown(path: &Path, report: &AbReport) -> Result<()> {
    write_text(path, &render_ab_markdown(report))
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n", text.trim_end()))
        .with_context(|| format!("writing {}", path.display()))
}

/// Render the report as Markdown: a provenance table, then one row per corpus
/// and thread count.
pub fn render_ab_markdown(report: &AbReport) -> String {
    let env = &report.environment;
    let settings = &report.settings;
    let threads: Vec<String> = settings.threads.iter().map(usize::to_string).collect();
    let budget = settings.budget;
    let mut out = String::from("# CodeGraph A/B comparison\n\n| Field | Value |\n|---|---|\n");
    let mut field = |name: &str, value: String| {
        out.push_str(&format!("| {name} | {} |\n", value.replace('|', "\\|")));
    };
    for (name, binary) in [
        ("Baseline", &report.baseline),
        ("Candidate", &report.candidate),
    ] {
        field(
            name,
            format!(
                "`{}` ({})",
                binary.path,
                binary.version.as_deref().unwrap_or("version unknown")
            ),
        );
        field(&format!("{name} sha256"), format!("`{}`", binary.sha256));
    }
    field(
        "Runs per cell",
        format!(
            "{} ({} warm-up discarded; {})",
            settings.runs, settings.discarded_warmup_runs, settings.order
        ),
    );
    field(
        &format!("Thread counts (`{}`)", settings.thread_env),
        threads.join(", "),
    );
    field(
        "CPU",
        format!(
            "{} ({} logical)",
            env.cpu_model.as_deref().unwrap_or("unknown"),
            env.logical_cpus
                .map_or_else(|| "?".to_string(), |cpus| cpus.to_string())
        ),
    );
    field(
        "Kernel",
        env.kernel.clone().unwrap_or_else(|| "unknown".into()),
    );
    field(
        "rustc",
        env.rustc_version
            .clone()
            .unwrap_or_else(|| "unknown".into()),
    );
    field(
        "Harness commit",
        match (&env.harness_commit, env.harness_dirty) {
            (Some(commit), Some(true)) => format!("`{commit}` (dirty)"),
            (Some(commit), _) => format!("`{commit}`"),
            (None, _) => "unknown".to_string(),
        },
    );
    field(
        "Budget",
        format!(
            "init wall ≤ {:.2}× (or rows grew and rows/s ≥ {:.2}×); peak RSS ≤ {:.2}×",
            budget.init_wall_ratio_max, budget.rows_per_second_ratio_min, budget.peak_rss_ratio_max
        ),
    );

    out.push_str(
        "\n| Corpus | Threads | Baseline ms (MAD) | Candidate ms (MAD) | Wall × | \
         Baseline RSS MiB | Candidate RSS MiB | RSS × | Rows × | Rows/s × | Graph | Budget |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---|\n",
    );
    for corpus in &report.corpora {
        let graph = if corpus.graph.identical {
            "identical".to_string()
        } else {
            format!("differs: {}", corpus.graph.differing_surfaces.join(", "))
        };
        for cell in &corpus.cells {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {graph} | {} |\n",
                corpus.name,
                cell.threads,
                summary_ms(&cell.baseline.wall_ms),
                summary_ms(&cell.candidate.wall_ms),
                ratio_text(cell.ratio.wall_ms),
                rss_mib(cell.baseline.peak_rss_kb),
                rss_mib(cell.candidate.peak_rss_kb),
                ratio_text(cell.ratio.peak_rss_kb),
                ratio_text(cell.ratio.canonical_rows),
                ratio_text(cell.ratio.rows_per_second),
                budget_text(&cell.budget),
            ));
        }
    }
    out
}

fn summary_ms(summary: &Summary) -> String {
    format!("{:.1} ({:.1})", summary.median, summary.mad)
}

fn rss_mib(summary: Option<Summary>) -> String {
    summary.map_or_else(
        || "n/a".to_string(),
        |rss| format!("{:.1}", rss.median / 1024.0),
    )
}

fn ratio_text(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "n/a".to_string(), |value| format!("{value:.3}"))
}

fn budget_text(verdict: &BudgetVerdict) -> String {
    let mut over = Vec::new();
    if verdict.init_wall_ok == Some(false) {
        over.push("init wall over");
    }
    if verdict.peak_rss_ok == Some(false) {
        over.push("peak RSS over");
    }
    if !over.is_empty() {
        return over.join(", ");
    }
    match (verdict.init_wall_ok, verdict.peak_rss_ok) {
        (Some(true), Some(true)) => "ok".to_string(),
        (Some(true), None) => "ok (no RSS)".to_string(),
        _ => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ratios(wall: f64, rows: f64, rate: f64, rss: Option<f64>) -> Ratios {
        Ratios {
            wall_ms: Some(wall),
            peak_rss_kb: rss,
            canonical_rows: Some(rows),
            rows_per_second: Some(rate),
        }
    }

    #[test]
    fn budget_allows_slower_init_only_when_rows_grew_and_throughput_held() {
        let limits = BudgetLimits::PROJECT;
        let verdict = |r: Ratios| BudgetVerdict::evaluate(&r, &limits);
        assert_eq!(
            verdict(ratios(1.10, 1.0, 0.91, Some(1.15))).init_wall_ok,
            Some(true)
        );
        assert_eq!(
            verdict(ratios(1.10, 1.0, 0.91, Some(1.15))).peak_rss_ok,
            Some(true)
        );
        assert_eq!(
            verdict(ratios(1.11, 1.0, 0.95, None)).init_wall_ok,
            Some(false)
        );
        assert_eq!(
            verdict(ratios(1.30, 1.25, 0.96, None)).init_wall_ok,
            Some(true)
        );
        assert_eq!(
            verdict(ratios(1.30, 1.10, 0.85, None)).init_wall_ok,
            Some(false)
        );
        assert_eq!(
            verdict(ratios(1.0, 1.0, 1.0, Some(1.16))).peak_rss_ok,
            Some(false)
        );
        assert_eq!(verdict(ratios(1.0, 1.0, 1.0, None)).peak_rss_ok, None);
    }

    #[test]
    fn warm_up_runs_are_left_out_of_the_statistics() {
        let sample = |run: usize, wall_ms: f64, rss: u64| RunSample {
            run,
            warmup: run == 1,
            wall_ms,
            peak_rss_kb: Some(rss),
            canonical_hash: "h".to_string(),
        };
        let series = ArmSeries::new(
            vec![
                sample(1, 1000.0, 9000),
                sample(2, 10.0, 100),
                sample(3, 14.0, 300),
                sample(4, 12.0, 200),
            ],
            Some(120),
        )
        .unwrap();
        assert_eq!(
            series.wall_ms,
            Summary {
                median: 12.0,
                mad: 2.0,
                samples: 3
            }
        );
        assert_eq!(series.peak_rss_kb.map(|rss| rss.median), Some(200.0));
        assert_eq!(series.rows_per_second, Some(10_000.0));
    }

    #[test]
    fn ratios_are_absent_rather_than_infinite() {
        assert_eq!(ratio(Some(2.0), Some(0.0)), None);
        assert_eq!(ratio(None, Some(1.0)), None);
        assert_eq!(ratio(Some(3.0), Some(2.0)), Some(1.5));
    }

    #[test]
    fn runs_alternate_which_binary_goes_first() {
        assert_eq!(Arm::order(0), [Arm::Baseline, Arm::Candidate]);
        assert_eq!(Arm::order(1), [Arm::Candidate, Arm::Baseline]);
        assert_eq!(Arm::order(4), [Arm::Baseline, Arm::Candidate]);
    }

    #[test]
    fn copies_leave_out_vcs_and_index_directories() {
        let scratch = ScratchDir::new("copy-test").unwrap();
        let source = scratch.path().join("source");
        for dir in [
            ".git",
            ".codegraph",
            ".codegraph-wsl",
            "src/.git",
            "src/nested",
        ] {
            fs::create_dir_all(source.join(dir)).unwrap();
            fs::write(source.join(dir).join("marker"), b"x").unwrap();
        }
        fs::write(source.join("src/lib.rs"), b"fn a() {}\n").unwrap();
        let destination = scratch.path().join("copy");
        let stats = copy_corpus(&source, &destination).unwrap();
        assert_eq!(stats.files, 2);
        assert!(destination.join("src/lib.rs").is_file());
        assert!(destination.join("src/nested/marker").is_file());
        for dir in [".git", ".codegraph", ".codegraph-wsl", "src/.git"] {
            assert!(!destination.join(dir).exists(), "{dir} must not be copied");
        }
    }
}
