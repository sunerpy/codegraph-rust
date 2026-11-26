//! `bench --ab`: A/B timing of CodeGraph binaries, a baseline and one or more
//! candidates.
//!
//! For every corpus, thread cell and run, the harness times one operation of
//! each binary:
//!
//! - `init` (the default): it copies the corpus subtree into a fresh temporary
//!   project (without `.git` or an index directory) and times
//!   `<binary> init <project>`;
//! - `sync-edit:<N>` and `sync-edit:1:<path>`: each binary gets its own copy of
//!   the corpus, indexed by an untimed `init`; a comment line is appended to
//!   the first `N` indexed files that take a line comment (in path order), or
//!   to `<path>`, before every `<binary> sync <project>`, of which one untimed
//!   warm-up sync comes first and the rest are timed. At the end of the cell
//!   every edited file is set to its original text plus one final comment, an
//!   untimed sync indexes that, and the synced graph must equal a fresh
//!   `init` of the same source, or the harness stops.
//!
//! Every child runs with `RAYON_NUM_THREADS=<threads>` (removed for the
//! `default` cell), `CODEGRAPH_NO_DAEMON=1`, `CODEGRAPH_NO_WATCH=1` and
//! `CODEGRAPH_PHASE_TIMINGS=1`, and with `CODEGRAPH_DIR` removed. The CLI has
//! no thread flag; parsing and resolution run on rayon's global pool, which
//! reads `RAYON_NUM_THREADS`. Each timed run records its wall time, its peak
//! RSS, its CPU time (from `wait4`), and the phase-timing lines of its stderr,
//! read before the run's directory is deleted. On Linux the peak RSS is the
//! child's own `VmHWM`, polled from `/proc/<pid>/status` while it runs:
//! `wait4`'s `ru_maxrss` also counts the address space the child was forked
//! from, the harness's own, which grows as it canonicalizes each database, so
//! every run after a large corpus read the harness's size. Other Unix hosts
//! report `ru_maxrss`.
//!
//! The binaries take turns: run `r` of a cell starts with arm `r mod n` of the
//! `n` arms (the baseline, then the candidates in order), so with one
//! candidate even runs time the baseline first and odd runs the candidate
//! first. The first run of every (corpus, binary, threads) cell is a warm-up
//! and is left out of the statistics. With the load guard on
//! ([`crate::load_guard`]), timing starts on a quiet host, and a run the
//! guard finds sharing its CPUs is void, kept in the report and run again.
//!
//! The harness fails closed and writes no report when a run exits non-zero, is
//! killed or times out; when an `init` exits 0 without a database, or with one
//! that fails `PRAGMA quick_check` or canonicalization; when two runs of the
//! same binary on one corpus, at any thread counts, produce different
//! canonical graphs; when a sync does not re-index the edited files or does
//! not converge with a fresh `init`; when a binary's phase-timing lines are
//! malformed or name different phases in different runs of a cell; and when a
//! cell cannot collect its runs within the guard's attempts. A baseline and
//! candidate graph difference is not an error: it is reported per corpus, so
//! a candidate that changes the graph can still be timed against its row
//! growth.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
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
    load_graph, sha256_file,
};
#[cfg(target_os = "linux")]
use crate::load_guard::ProcProbe;
use crate::load_guard::{LoadGuard, LoadProbe, SessionStart};
#[cfg(unix)]
use crate::metrics::read_proc_status_kb;
use crate::metrics::{mad, median};
use crate::oracle::CanonicalRow;
use crate::report::{command_stdout, read_cpu_model, read_mem_total_kb, read_os_pretty_name};

/// Environment variable that sets the thread count of each timed run.
pub const THREAD_ENV: &str = "RAYON_NUM_THREADS";
/// Environment variable that makes CodeGraph print its phase timings.
pub const PHASE_TIMINGS_ENV: &str = "CODEGRAPH_PHASE_TIMINGS";
/// The first field of a phase-timing line (`docs/cli.md`, Index diagnostics).
pub const PHASE_TAG: &str = "codegraph-phase";
/// Default wall-clock cap for one timed run; a run past it is killed and fails.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Regression budget (`docs/benchmark.md`): candidate/baseline cold-`init`
/// median wall time.
pub const INIT_WALL_RATIO_MAX: f64 = 1.10;
/// Regression budget: candidate/baseline single-file `sync` median wall time.
pub const SYNC_WALL_RATIO_MAX: f64 = 1.15;
/// A slower `init` is still within budget when the candidate graph has more
/// canonical rows and its rows/s stay at or above this ratio.
pub const ROWS_PER_SECOND_RATIO_MIN: f64 = 0.90;
/// Regression budget: candidate/baseline median peak RSS.
pub const PEAK_RSS_RATIO_MAX: f64 = 1.15;

const REPORT_KIND: &str = "codegraph-ab";
const REPORT_SCHEMA_VERSION: u32 = 2;
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const STDERR_TAIL_LINES: usize = 40;
const DATABASE_FILE: &str = "codegraph.db";
/// The text of every comment line a sync edit appends, after the comment mark.
const SYNC_EDIT_MARK: &str = "codegraph-bench sync-edit";

/// One thread cell: a `RAYON_NUM_THREADS` value, or the variable unset.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Threads {
    Count(usize),
    /// `RAYON_NUM_THREADS` removed: rayon sizes its pool to the CPUs.
    Default,
}

impl Threads {
    /// `default`, or a thread count of at least 1.
    pub fn parse(text: &str) -> Result<Self> {
        if text == "default" {
            return Ok(Self::Default);
        }
        let count: usize = text
            .parse()
            .with_context(|| format!("a thread cell is a count or `default`, not {text:?}"))?;
        Ok(Self::Count(count))
    }

    pub fn count(self) -> Option<usize> {
        match self {
            Self::Count(count) => Some(count),
            Self::Default => None,
        }
    }

    /// The thread count the cell's children run with.
    pub fn resolved(self) -> usize {
        self.count().unwrap_or_else(logical_cpus)
    }

    pub fn label(self) -> String {
        match self {
            Self::Count(count) => count.to_string(),
            Self::Default => "default".to_string(),
        }
    }
}

impl fmt::Display for Threads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(count) => write!(f, "{THREAD_ENV}={count}"),
            Self::Default => write!(f, "{THREAD_ENV} unset"),
        }
    }
}

/// The CPUs this process may run on, which a child's rayon pool takes by
/// default.
pub fn logical_cpus() -> usize {
    thread::available_parallelism().map_or(1, usize::from)
}

/// What each timed run does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AbOp {
    /// A cold `init` of a fresh copy of the corpus.
    Init,
    /// A `sync` after a comment line is appended to the first `files` indexed
    /// files that take a line comment, in path order.
    SyncEdit { files: usize },
    /// A `sync` after a comment line is appended to the indexed file `path`.
    SyncEditFile { path: String },
}

impl AbOp {
    /// `init`, `sync-edit:<N>` or `sync-edit:1:<path>`.
    pub fn parse(text: &str) -> Result<Self> {
        if text == "init" {
            return Ok(Self::Init);
        }
        let Some(rest) = text.strip_prefix("sync-edit:") else {
            bail!("--op is `init`, `sync-edit:<N>` or `sync-edit:1:<path>`, not {text:?}");
        };
        match rest.split_once(':') {
            None => {
                let files: usize = rest
                    .parse()
                    .with_context(|| format!("sync-edit takes a file count, not {rest:?}"))?;
                if files == 0 {
                    bail!("sync-edit edits at least one file");
                }
                Ok(Self::SyncEdit { files })
            }
            Some((count, path)) => {
                if count != "1" {
                    bail!("a named file is edited on its own: sync-edit:1:<path>, not {text:?}");
                }
                if path.is_empty()
                    || path.starts_with('/')
                    || path.contains('\\')
                    || path
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
                {
                    bail!("sync-edit names a project-relative path with `/`, not {path:?}");
                }
                Ok(Self::SyncEditFile {
                    path: path.to_string(),
                })
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Init => "init".to_string(),
            Self::SyncEdit { files } => format!("sync-edit:{files}"),
            Self::SyncEditFile { path } => format!("sync-edit:1:{path}"),
        }
    }

    /// The CodeGraph command each timed run runs.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::SyncEdit { .. } | Self::SyncEditFile { .. } => "sync",
        }
    }

    fn is_sync(&self) -> bool {
        !matches!(self, Self::Init)
    }

    /// Whether the run is a single-file sync, which the sync budget covers.
    fn single_file_sync(&self) -> bool {
        matches!(
            self,
            Self::SyncEdit { files: 1 } | Self::SyncEditFile { .. }
        )
    }
}

/// What one `bench --ab` invocation compares.
#[derive(Clone, Debug)]
pub struct AbConfig {
    pub baseline: PathBuf,
    /// One or more candidates, each compared with the baseline.
    pub candidates: Vec<PathBuf>,
    pub corpora: Vec<AbCorpus>,
    pub threads: Vec<Threads>,
    /// Runs per (corpus, binary, threads) cell, including the warm-up.
    pub runs: usize,
    pub run_timeout: Duration,
    pub op: AbOp,
    /// The load guard's limits; `None` times without it.
    pub load_guard: Option<LoadGuard>,
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
    /// The load guard's quiet start, when the guard was on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start: Option<SessionStart>,
    pub baseline: BinaryInfo,
    /// The first candidate; every candidate is in `candidates`.
    pub candidate: BinaryInfo,
    #[serde(default)]
    pub candidates: Vec<BinaryInfo>,
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

fn init_op() -> String {
    "init".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AbSettings {
    pub invocation: String,
    #[serde(default = "init_op")]
    pub op: String,
    pub env_set: BTreeMap<String, String>,
    pub env_removed: Vec<String>,
    pub thread_env: String,
    /// The thread count of each cell; a `default` cell's is the CPUs'.
    pub threads: Vec<usize>,
    /// Each cell as given: a count, or `default`.
    #[serde(default)]
    pub thread_settings: Vec<String>,
    pub runs: usize,
    pub discarded_warmup_runs: usize,
    pub order: String,
    pub run_timeout_secs: u64,
    pub budget: BudgetLimits,
    #[serde(default)]
    pub load_guard: Option<LoadGuard>,
}

fn sync_wall_ratio_max() -> f64 {
    SYNC_WALL_RATIO_MAX
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct BudgetLimits {
    pub init_wall_ratio_max: f64,
    #[serde(default = "sync_wall_ratio_max")]
    pub sync_wall_ratio_max: f64,
    pub rows_per_second_ratio_min: f64,
    pub peak_rss_ratio_max: f64,
}

impl BudgetLimits {
    pub const PROJECT: Self = Self {
        init_wall_ratio_max: INIT_WALL_RATIO_MAX,
        sync_wall_ratio_max: SYNC_WALL_RATIO_MAX,
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
    /// The first candidate's graph against the baseline's; every
    /// candidate's is in `graphs`.
    pub graph: GraphComparison,
    #[serde(default)]
    pub graphs: Vec<GraphComparison>,
    /// The files a sync run edits, in path order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_files: Vec<String>,
    pub cells: Vec<Cell>,
}

/// One candidate's graph against the baseline's. For a sync run it is the
/// graph each binary's last sync left, which equals its fresh `init` of the
/// same source.
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
    /// Whether the cell left `RAYON_NUM_THREADS` unset (`--threads default`).
    #[serde(default)]
    pub default_threads: bool,
    pub baseline: ArmSeries,
    /// The first candidate's runs, ratios and budget; every candidate's are
    /// in `candidates`.
    pub candidate: ArmSeries,
    pub ratio: Ratios,
    pub budget: BudgetVerdict,
    #[serde(default)]
    pub candidates: Vec<CandidateCell>,
}

/// One candidate in a cell, against the baseline.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandidateCell {
    pub series: ArmSeries,
    pub ratio: Ratios,
    pub budget: BudgetVerdict,
    /// Candidate/baseline median of each phase both report.
    #[serde(default)]
    pub phase_ratio: BTreeMap<String, f64>,
}

/// Every run of one binary in a cell, and the statistics of the measured ones:
/// neither the warm-up nor a void run.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArmSeries {
    pub runs: Vec<RunSample>,
    pub wall_ms: Summary,
    pub peak_rss_kb: Option<Summary>,
    pub rows_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<Summary>,
    /// The foreign CPUs the load guard measured over the measured runs; each
    /// run's own value, a void run's included, is in its `load`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_cpus: Option<Summary>,
    /// Each phase the binary reported, in milliseconds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phase_ms: BTreeMap<String, Summary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunSample {
    /// 1-based attempt number within the cell; a void run's rerun has the
    /// next one.
    pub run: usize,
    pub warmup: bool,
    pub wall_ms: f64,
    pub peak_rss_kb: Option<u64>,
    /// The canonical graph an `init` run wrote; a sync run's graph is checked
    /// once, at the end of the cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_hash: Option<String>,
    /// The child's user and system time, from `wait4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<f64>,
    /// The child's phase-timing lines, in milliseconds by phase.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phase_ms: BTreeMap<String, f64>,
    /// What the load guard saw around the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<RunLoad>,
    /// Why the load guard voided the run; a void run counts in no statistic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void: Option<String>,
}

/// The load guard's view of one run.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunLoad {
    /// The 1-minute load when the run started.
    pub load1: f64,
    /// The idle CPUs of the sample before the run.
    pub idle_cpus: f64,
    /// How long the run waited for the host.
    pub waited_ms: f64,
    /// The average CPUs other work used during the run.
    pub foreign_cpus: f64,
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

/// The regression budget applied to one cell; `None` when not measurable or
/// not the cell's operation.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct BudgetVerdict {
    pub init_wall_ok: Option<bool>,
    #[serde(default)]
    pub sync_wall_ok: Option<bool>,
    pub peak_rss_ok: Option<bool>,
}

impl BudgetVerdict {
    /// The `init` budget: wall time and peak RSS.
    pub fn evaluate(ratio: &Ratios, limits: &BudgetLimits) -> Self {
        let init_wall_ok = ratio.wall_ms.map(|wall| {
            let rows_grew = ratio.canonical_rows.is_some_and(|rows| rows > 1.0);
            let throughput_held = ratio
                .rows_per_second
                .is_some_and(|rate| rate >= limits.rows_per_second_ratio_min);
            wall <= limits.init_wall_ratio_max || (rows_grew && throughput_held)
        });
        Self {
            init_wall_ok,
            sync_wall_ok: None,
            peak_rss_ok: Self::rss_ok(ratio, limits),
        }
    }

    /// The sync budget: a single-file sync's wall time, and peak RSS.
    pub fn evaluate_sync(ratio: &Ratios, limits: &BudgetLimits, single_file: bool) -> Self {
        Self {
            init_wall_ok: None,
            sync_wall_ok: ratio
                .wall_ms
                .filter(|_| single_file)
                .map(|wall| wall <= limits.sync_wall_ratio_max),
            peak_rss_ok: Self::rss_ok(ratio, limits),
        }
    }

    fn rss_ok(ratio: &Ratios, limits: &BudgetLimits) -> Option<bool> {
        ratio
            .peak_rss_kb
            .map(|rss| rss <= limits.peak_rss_ratio_max)
    }
}

/// Time every binary on every configured corpus, with the load guard on the
/// host's `/proc` when the configuration asks for it.
pub fn run_ab(config: &AbConfig) -> Result<AbReport> {
    if config.load_guard.is_none() {
        return run_ab_with_probe(config, None);
    }
    #[cfg(target_os = "linux")]
    {
        let mut probe = ProcProbe::new()?;
        run_ab_with_probe(config, Some(&mut probe))
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("the load guard reads /proc, which this host lacks: pass --no-load-guard")
    }
}

/// [`run_ab`] with the load guard reading `probe`; `None` when the guard is
/// off.
pub fn run_ab_with_probe(config: &AbConfig, probe: Option<&mut dyn LoadProbe>) -> Result<AbReport> {
    validate(config)?;
    let guard = match (&config.load_guard, probe) {
        (Some(limits), Some(probe)) => Some((limits, probe)),
        (None, _) => None,
        (Some(_), None) => bail!("the load guard needs a probe"),
    };
    let scratch = ScratchDir::new("ab")?;
    let baseline = BinaryInfo::inspect(&config.baseline, &scratch.path().join("baseline.version"))
        .context("inspecting the --baseline binary")?;
    let candidates = config
        .candidates
        .iter()
        .enumerate()
        .map(|(index, path)| {
            BinaryInfo::inspect(
                path,
                &scratch.path().join(format!("candidate{index}.version")),
            )
            .with_context(|| format!("inspecting the --candidate binary {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session {
        config,
        binaries: std::iter::once(&baseline).chain(&candidates).collect(),
        scratch: scratch.path(),
        guard,
    };
    let session_start = match &mut session.guard {
        Some((limits, probe)) => {
            eprintln!(
                "==> load guard: waiting for {} quiet checks in a row",
                limits.start_checks
            );
            let start = limits.wait_for_quiet_start(&mut **probe)?;
            eprintln!(
                "    quiet after {} checks ({:.0} s)",
                start.checks.len(),
                start.waited_secs
            );
            Some(start)
        }
        None => None,
    };
    let mut corpora = Vec::with_capacity(config.corpora.len());
    for (index, corpus) in config.corpora.iter().enumerate() {
        eprintln!("==> {} ({})", corpus.name, corpus.source.display());
        corpora.push(session.measure_corpus(index, corpus)?);
    }
    Ok(AbReport {
        schema_version: REPORT_SCHEMA_VERSION,
        kind: REPORT_KIND.to_string(),
        argv: config.argv.clone(),
        environment: AbEnvironment::detect(config.workspace_root.as_deref()),
        settings: AbSettings::of(config),
        session_start,
        candidate: candidates[0].clone(),
        baseline,
        candidates,
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
    if config.candidates.is_empty() {
        bail!("--ab needs at least one --candidate");
    }
    if config.threads.is_empty() {
        bail!("--ab needs at least one thread count (--threads <n[,m]>)");
    }
    if config.threads.contains(&Threads::Count(0)) {
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
    if let Some(guard) = &config.load_guard
        && (guard.logical_cpus == 0 || guard.max_attempts_factor == 0)
    {
        bail!("the load guard needs a CPU count and at least one attempt per run");
    }
    Ok(())
}

/// The arms of run `run` in the order they go: the order rotates by one arm a
/// run, so each of the `arms` arms goes first equally often. With two arms the
/// baseline goes first in even runs and the candidate in odd ones.
fn rotation(run: usize, arms: usize) -> Vec<usize> {
    (0..arms).map(|offset| (run + offset) % arms).collect()
}

/// An arm's name in progress lines and errors: `baseline`, `candidate` when
/// there is one, `candidate <i>` when there are several.
fn arm_name(arm: usize, candidates: usize) -> String {
    match arm {
        0 => "baseline".to_string(),
        _ if candidates == 1 => "candidate".to_string(),
        index => format!("candidate {index}"),
    }
}

/// Names one timed run in progress lines and errors.
struct RunLabel<'a> {
    arm: &'a str,
    corpus: &'a str,
    threads: Threads,
    run: usize,
    runs: usize,
}

impl fmt::Display for RunLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} run {}/{} on {} ({})",
            self.arm,
            self.run + 1,
            self.runs,
            self.corpus,
            self.threads
        )
    }
}

/// One `bench --ab` invocation in progress: the binaries in arm order (the
/// baseline first) and the load guard with its probe.
struct Session<'a, 'p> {
    config: &'a AbConfig,
    binaries: Vec<&'a BinaryInfo>,
    scratch: &'a Path,
    guard: Option<(&'a LoadGuard, &'p mut dyn LoadProbe)>,
}

/// What one timed run measured.
struct TimedRun {
    measured: Measured,
    phases: BTreeMap<String, f64>,
    /// The graph an `init` run wrote.
    graph: Option<GraphFingerprint>,
    copied: CopyStats,
    load: Option<RunLoad>,
    void: Option<String>,
}

impl Session<'_, '_> {
    fn arms(&self) -> usize {
        self.binaries.len()
    }

    fn arm_name(&self, arm: usize) -> String {
        arm_name(arm, self.arms() - 1)
    }

    /// The most attempts a cell gives one binary.
    fn max_attempts(&self) -> usize {
        let factor = self
            .guard
            .as_ref()
            .map_or(1, |(limits, _)| limits.max_attempts_factor);
        self.config.runs * factor
    }

    fn measure_corpus(&mut self, index: usize, corpus: &AbCorpus) -> Result<CorpusAb> {
        let arms = self.arms();
        let config = self.config;
        let mut graphs: Vec<Option<GraphFingerprint>> = vec![None; arms];
        let mut copied = CopyStats::default();
        let mut edited_files = Vec::new();
        let mut cells = Vec::with_capacity(config.threads.len());
        for &threads in &config.threads {
            let cell_dir = self.scratch.join(format!("c{index}-t{}", threads.label()));
            fs::create_dir_all(&cell_dir)
                .with_context(|| format!("creating {}", cell_dir.display()))?;
            let mut syncs: Vec<Option<SyncArm>> = Vec::with_capacity(arms);
            for arm in 0..arms {
                syncs.push(if config.op.is_sync() {
                    let (sync, stats) = self
                        .prepare_sync_arm(arm, corpus, threads, &cell_dir)
                        .with_context(|| {
                            format!(
                                "preparing the {} sync runs on {} ({threads})",
                                self.arm_name(arm),
                                corpus.name
                            )
                        })?;
                    copied = stats;
                    edited_files = sync.relative_paths();
                    Some(sync)
                } else {
                    None
                });
            }
            let mut series: Vec<Vec<RunSample>> = vec![Vec::new(); arms];
            let mut attempts = vec![0_usize; arms];
            for run in 0..config.runs {
                for arm in rotation(run, arms) {
                    let name = self.arm_name(arm);
                    let label = RunLabel {
                        arm: &name,
                        corpus: &corpus.name,
                        threads,
                        run,
                        runs: config.runs,
                    };
                    loop {
                        attempts[arm] += 1;
                        if attempts[arm] > self.max_attempts() {
                            bail!(
                                "{label}: the load guard voided too many runs; {} attempts \
                                 left the cell short of {} valid runs, so no report is written",
                                self.max_attempts(),
                                config.runs
                            );
                        }
                        let attempt = attempts[arm];
                        let timed = match &mut syncs[arm] {
                            Some(sync) => self.timed_sync(arm, sync, threads, run == 0, attempt),
                            None => {
                                self.timed_init(arm, corpus, threads, &cell_dir, run == 0, attempt)
                            }
                        }
                        .with_context(|| format!("{label} failed"))?;
                        if let Some(graph) = &timed.graph {
                            check_same_graph(
                                &mut graphs[arm],
                                graph,
                                &label.to_string(),
                                &name,
                                &corpus.name,
                            )?;
                            copied = timed.copied;
                        }
                        let sample = RunSample {
                            run: attempt,
                            warmup: run == 0,
                            wall_ms: timed.measured.wall.as_secs_f64() * 1000.0,
                            peak_rss_kb: timed.measured.peak_rss_kb,
                            canonical_hash: timed.graph.map(|graph| graph.hash),
                            cpu_ms: timed.measured.cpu.map(|cpu| cpu.as_secs_f64() * 1000.0),
                            phase_ms: timed.phases,
                            load: timed.load,
                            void: timed.void,
                        };
                        eprintln!(
                            "    {label}: {:.1} ms, peak RSS {}{}{}",
                            sample.wall_ms,
                            sample
                                .peak_rss_kb
                                .map_or_else(|| "n/a".to_string(), |kb| format!("{kb} KiB")),
                            if run == 0 { " (warm-up)" } else { "" },
                            sample.void.as_ref().map_or_else(String::new, |why| format!(
                                " (void: {why}; running again)"
                            ))
                        );
                        let void = sample.void.is_some();
                        series[arm].push(sample);
                        if !void {
                            break;
                        }
                    }
                }
            }
            for (arm, sync) in syncs.into_iter().enumerate() {
                let Some(sync) = sync else { continue };
                let name = self.arm_name(arm);
                let graph = self
                    .finish_sync_arm(arm, sync, corpus, threads)
                    .with_context(|| {
                        format!(
                            "finishing the {name} sync runs on {} ({threads})",
                            corpus.name
                        )
                    })?;
                let label = format!("the {name} final sync on {} ({threads})", corpus.name);
                check_same_graph(&mut graphs[arm], &graph, &label, &name, &corpus.name)?;
            }
            fs::remove_dir_all(&cell_dir)
                .with_context(|| format!("removing {}", cell_dir.display()))?;
            for (arm, runs) in series.iter().enumerate() {
                check_phase_names(runs).with_context(|| {
                    format!(
                        "the {} runs on {} ({threads})",
                        self.arm_name(arm),
                        corpus.name
                    )
                })?;
            }
            let rows: Vec<Option<u64>> = graphs
                .iter()
                .map(|graph| graph.as_ref().map(|graph| graph.rows.total))
                .collect();
            cells.push(build_cell(threads, series, &rows, &config.op)?);
        }
        let graphs = graphs
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .with_context(|| format!("corpus {} produced no measured runs", corpus.name))?;
        let comparisons: Vec<GraphComparison> = graphs[1..]
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let differing = differing_surfaces(&graphs[0], candidate);
                eprintln!(
                    "    graph: {}",
                    if differing.is_empty() {
                        format!("{} identical", self.arm_name(index + 1))
                    } else {
                        format!(
                            "baseline and {} differ in {}",
                            self.arm_name(index + 1),
                            differing.join(", ")
                        )
                    }
                );
                GraphComparison {
                    identical: differing.is_empty(),
                    differing_surfaces: differing,
                    baseline: graphs[0].clone(),
                    candidate: candidate.clone(),
                }
            })
            .collect();
        Ok(CorpusAb {
            name: corpus.name.clone(),
            commit: corpus.commit.clone(),
            tag: corpus.tag.clone(),
            source: corpus.source.display().to_string(),
            copied_files: copied.files,
            skipped_symlinks: copied.skipped_symlinks,
            graph: comparisons[0].clone(),
            graphs: comparisons,
            edited_files,
            cells,
        })
    }

    /// Run `spawn` (the timed child alone) inside the load guard: wait for the
    /// host, then measure the CPU time others used. A warm-up is never void.
    fn guarded(
        &mut self,
        threads: Threads,
        warmup: bool,
        spawn: impl FnOnce() -> Result<Measured>,
    ) -> Result<(Measured, Option<RunLoad>, Option<String>)> {
        let Some((limits, probe)) = &mut self.guard else {
            return Ok((spawn()?, None, None));
        };
        let saturating = limits.saturating(threads.count());
        let gate = limits.wait_before_run(&mut **probe, saturating)?;
        let window = limits.open_window(&mut **probe)?;
        let measured = spawn()?;
        let foreign = limits.foreign_cpus(
            &mut **probe,
            window,
            measured.cpu.unwrap_or_default(),
            measured.wall,
        )?;
        let limit = limits.foreign_cpus_limit(saturating);
        let void = (!warmup && foreign > limit)
            .then(|| format!("{foreign:.2} foreign CPUs, over the {limit} limit"));
        let load = RunLoad {
            load1: gate.load1,
            idle_cpus: gate.idle_cpus,
            waited_ms: gate.waited.as_secs_f64() * 1000.0,
            foreign_cpus: foreign,
        };
        Ok((measured, Some(load), void))
    }

    /// One timed cold `init` of a fresh copy of the corpus.
    fn timed_init(
        &mut self,
        arm: usize,
        corpus: &AbCorpus,
        threads: Threads,
        cell_dir: &Path,
        warmup: bool,
        attempt: usize,
    ) -> Result<TimedRun> {
        let binary = self.binaries[arm];
        let run_dir = cell_dir.join(format!("arm{arm}-r{attempt}"));
        let project = run_dir.join("project");
        fs::create_dir_all(&run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
        let copied = copy_corpus(&corpus.source, &project)?;
        let stdout = run_dir.join("init.stdout");
        let stderr = run_dir.join("init.stderr");
        let mut command = codegraph_command(
            binary, "init", &project, threads, &run_dir, &stdout, &stderr,
        )?;
        let timeout = self.config.run_timeout;
        let (measured, load, void) = self.guarded(threads, warmup, || {
            spawn_measured(&mut command, timeout)
                .with_context(|| format!("running {} init", binary.path))
        })?;
        check_exit(binary, "init", &measured, &stderr, timeout)?;
        let phases = read_phases(&stderr, "init")?;
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
        fs::remove_dir_all(&run_dir).with_context(|| format!("removing {}", run_dir.display()))?;
        Ok(TimedRun {
            measured,
            phases,
            graph: Some(fingerprint(&canonical)),
            copied,
            load,
            void,
        })
    }

    /// Index the arm's own copy of the corpus, pick the files to edit, and
    /// run the untimed warm-up sync.
    fn prepare_sync_arm(
        &mut self,
        arm: usize,
        corpus: &AbCorpus,
        threads: Threads,
        cell_dir: &Path,
    ) -> Result<(SyncArm, CopyStats)> {
        let binary = self.binaries[arm];
        let dir = cell_dir.join(format!("arm{arm}"));
        let project = dir.join("project");
        let copied = copy_corpus(&corpus.source, &project)?;
        let timeout = self.config.run_timeout;
        run_untimed(binary, "init", &project, threads, &dir, timeout)?;
        let db = project.join(DEFAULT_CURRENT_DIR).join(DATABASE_FILE);
        if !db.is_file() {
            bail!(
                "`{} init` exited 0 but produced no database at {}",
                binary.path,
                db.display()
            );
        }
        let (_, graph) = load_graph(&db)?;
        let edits = select_edits(&graph.files, &self.config.op, &project)?;
        let mut sync = SyncArm {
            dir,
            project,
            edits,
        };
        sync.append("warm-up")?;
        let stdout = run_untimed(binary, "sync", &sync.project, threads, &sync.dir, timeout)?;
        check_reindexed(binary, &stdout, sync.edits.len())?;
        Ok((sync, copied))
    }

    /// One timed sync after another comment line in every edited file.
    fn timed_sync(
        &mut self,
        arm: usize,
        sync: &mut SyncArm,
        threads: Threads,
        warmup: bool,
        attempt: usize,
    ) -> Result<TimedRun> {
        let binary = self.binaries[arm];
        sync.append(&attempt.to_string())?;
        let stdout = sync.dir.join(format!("sync-{attempt}.stdout"));
        let stderr = sync.dir.join(format!("sync-{attempt}.stderr"));
        let mut command = codegraph_command(
            binary,
            "sync",
            &sync.project,
            threads,
            &sync.dir,
            &stdout,
            &stderr,
        )?;
        let timeout = self.config.run_timeout;
        let (measured, load, void) = self.guarded(threads, warmup, || {
            spawn_measured(&mut command, timeout)
                .with_context(|| format!("running {} sync", binary.path))
        })?;
        check_exit(binary, "sync", &measured, &stderr, timeout)?;
        let phases = read_phases(&stderr, "sync")?;
        let printed =
            fs::read_to_string(&stdout).with_context(|| format!("reading {}", stdout.display()))?;
        check_reindexed(binary, &printed, sync.edits.len())?;
        for path in [&stdout, &stderr] {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
        Ok(TimedRun {
            measured,
            phases,
            graph: None,
            copied: CopyStats::default(),
            load,
            void,
        })
    }

    /// Set every edited file to its original text plus one final comment,
    /// sync that, and check the synced graph against a fresh `init` of the
    /// same source: the graph every binary's runs of the cell end with.
    fn finish_sync_arm(
        &mut self,
        arm: usize,
        sync: SyncArm,
        corpus: &AbCorpus,
        threads: Threads,
    ) -> Result<GraphFingerprint> {
        let binary = self.binaries[arm];
        let timeout = self.config.run_timeout;
        sync.write_final()?;
        let stdout = run_untimed(binary, "sync", &sync.project, threads, &sync.dir, timeout)?;
        check_reindexed(binary, &stdout, sync.edits.len())?;
        let (_, synced) = load_graph(&sync.project.join(DEFAULT_CURRENT_DIR).join(DATABASE_FILE))?;
        let fresh = sync.dir.join("fresh");
        copy_corpus(&sync.project, &fresh)?;
        run_untimed(binary, "init", &fresh, threads, &sync.dir, timeout)?;
        let fresh_db = fresh.join(DEFAULT_CURRENT_DIR).join(DATABASE_FILE);
        let fresh_graph = canonicalize_checked(&fresh_db).with_context(|| {
            format!(
                "the database {} cannot be canonicalized",
                fresh_db.display()
            )
        })?;
        let (synced, fresh_graph) = (fingerprint(&synced), fingerprint(&fresh_graph));
        if synced.hash != fresh_graph.hash {
            bail!(
                "`{} sync` on {} does not converge with a fresh init of the edited source \
                 (differing surfaces: {}); a sync's timing is only measured on a sync that \
                 builds the graph init builds",
                binary.path,
                corpus.name,
                differing_surfaces(&synced, &fresh_graph).join(", ")
            );
        }
        fs::remove_dir_all(&sync.dir)
            .with_context(|| format!("removing {}", sync.dir.display()))?;
        Ok(synced)
    }
}

/// The statistics of one cell, its candidates against the baseline.
fn build_cell(
    threads: Threads,
    series: Vec<Vec<RunSample>>,
    rows: &[Option<u64>],
    op: &AbOp,
) -> Result<Cell> {
    let mut arms = series
        .into_iter()
        .zip(rows)
        .map(|(runs, rows)| ArmSeries::new(runs, rows.filter(|_| !op.is_sync())))
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let baseline = arms.next().context("a cell has a baseline")?;
    let candidates: Vec<CandidateCell> = arms
        .enumerate()
        .map(|(index, series)| {
            let ratio = Ratios::between(&baseline, &series, rows[0], rows[index + 1]);
            let budget = if op.is_sync() {
                BudgetVerdict::evaluate_sync(&ratio, &BudgetLimits::PROJECT, op.single_file_sync())
            } else {
                BudgetVerdict::evaluate(&ratio, &BudgetLimits::PROJECT)
            };
            let phase_ratio = baseline
                .phase_ms
                .iter()
                .filter_map(|(phase, base)| {
                    let candidate = series.phase_ms.get(phase)?;
                    Some((
                        phase.clone(),
                        ratio_of(Some(candidate.median), Some(base.median))?,
                    ))
                })
                .collect();
            CandidateCell {
                series,
                ratio,
                budget,
                phase_ratio,
            }
        })
        .collect();
    let first = candidates.first().context("a cell has a candidate")?;
    Ok(Cell {
        threads: threads.resolved(),
        default_threads: threads == Threads::Default,
        candidate: first.series.clone(),
        ratio: first.ratio,
        budget: first.budget,
        baseline,
        candidates,
    })
}

/// Every run of one binary in a cell must name the same phases: a set that
/// changes between runs means the runs did not do the same work.
fn check_phase_names(runs: &[RunSample]) -> Result<()> {
    let Some(first) = runs.first() else {
        return Ok(());
    };
    let names = |run: &RunSample| run.phase_ms.keys().cloned().collect::<BTreeSet<_>>();
    let expected = names(first);
    for run in &runs[1..] {
        let got = names(run);
        if got != expected {
            bail!(
                "run {} and run {} report different phases: {:?}",
                first.run,
                run.run,
                got.symmetric_difference(&expected).collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

fn check_same_graph(
    first: &mut Option<GraphFingerprint>,
    graph: &GraphFingerprint,
    label: &str,
    arm: &str,
    corpus: &str,
) -> Result<()> {
    match first {
        None => {
            *first = Some(graph.clone());
            Ok(())
        }
        Some(expected) if expected.hash == graph.hash => Ok(()),
        Some(expected) => bail!(
            "non-deterministic graph: {label} produced canonical hash {}, but the first {arm} run \
             on {corpus} produced {} (differing surfaces: {}); every run of one binary must \
             produce the same canonical graph",
            graph.hash,
            expected.hash,
            differing_surfaces(expected, graph).join(", ")
        ),
    }
}

/// One binary's project for a cell's sync runs.
struct SyncArm {
    dir: PathBuf,
    project: PathBuf,
    edits: Vec<EditTarget>,
}

/// A file the sync runs edit.
struct EditTarget {
    relative: String,
    full: PathBuf,
    original: Vec<u8>,
    comment: &'static str,
}

impl SyncArm {
    fn relative_paths(&self) -> Vec<String> {
        self.edits
            .iter()
            .map(|edit| edit.relative.clone())
            .collect()
    }

    /// Append one comment line, `<comment>codegraph-bench sync-edit <label>`,
    /// to every edited file.
    fn append(&mut self, label: &str) -> Result<()> {
        for edit in &self.edits {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&edit.full)
                .with_context(|| format!("opening {} to edit it", edit.full.display()))?;
            write!(file, "\n{}{SYNC_EDIT_MARK} {label}\n", edit.comment)
                .with_context(|| format!("editing {}", edit.full.display()))?;
        }
        Ok(())
    }

    /// Set every edited file to its original text and one final comment line,
    /// the same whatever number of runs came before.
    fn write_final(&self) -> Result<()> {
        for edit in &self.edits {
            let mut text = edit.original.clone();
            text.extend_from_slice(
                format!("\n{}{SYNC_EDIT_MARK} final\n", edit.comment).as_bytes(),
            );
            fs::write(&edit.full, text)
                .with_context(|| format!("editing {}", edit.full.display()))?;
        }
        Ok(())
    }
}

/// The line comment a sync edit appends to a file of `language`; `None` for a
/// language it leaves alone, one without line comments or whose files can end
/// outside code (PHP's HTML mode, the template and markup languages).
pub fn line_comment(language: &str) -> Option<&'static str> {
    match language {
        "typescript" | "javascript" | "tsx" | "jsx" | "arkts" | "go" | "rust" | "java" | "c"
        | "cpp" | "csharp" | "swift" | "kotlin" | "dart" | "scala" | "objc" | "solidity" => {
            Some("// ")
        }
        "python" | "ruby" | "r" | "nix" | "yaml" | "terraform" | "properties" | "gdscript" => {
            Some("# ")
        }
        "lua" | "luau" => Some("-- "),
        "erlang" => Some("% "),
        _ => None,
    }
}

/// The files a sync op edits, from the indexed files' canonical rows.
fn select_edits(files: &[CanonicalRow], op: &AbOp, project: &Path) -> Result<Vec<EditTarget>> {
    let mut indexed: Vec<(&str, &str)> = files
        .iter()
        .filter_map(|row| Some((row.get("path")?.as_str()?, row.get("language")?.as_str()?)))
        .collect();
    indexed.sort_unstable();
    let chosen: Vec<(&str, &'static str)> = match op {
        AbOp::Init => bail!("an init run edits no file"),
        AbOp::SyncEdit { files } => {
            let chosen: Vec<_> = indexed
                .iter()
                .filter_map(|(path, language)| Some((*path, line_comment(language)?)))
                .take(*files)
                .collect();
            if chosen.len() < *files {
                bail!(
                    "sync-edit:{files} needs {files} indexed files that take a line comment; \
                     the corpus has {}",
                    chosen.len()
                );
            }
            chosen
        }
        AbOp::SyncEditFile { path } => {
            let (_, language) = indexed
                .iter()
                .find(|(indexed, _)| indexed == path)
                .with_context(|| format!("{path} is not an indexed file of the corpus"))?;
            let comment = line_comment(language)
                .with_context(|| format!("{path} is {language}, which sync-edit does not edit"))?;
            vec![(path.as_str(), comment)]
        }
    };
    chosen
        .into_iter()
        .map(|(relative, comment)| {
            let full = project.join(relative);
            let original =
                fs::read(&full).with_context(|| format!("reading {}", full.display()))?;
            Ok(EditTarget {
                relative: relative.to_string(),
                full,
                original,
                comment,
            })
        })
        .collect()
}

/// `<binary> <command> <project>` with the environment every run gets, its
/// output to the two files.
fn codegraph_command(
    binary: &BinaryInfo,
    command_name: &str,
    project: &Path,
    threads: Threads,
    cwd: &Path,
    stdout: &Path,
    stderr: &Path,
) -> Result<Command> {
    let stdout_file =
        File::create(stdout).with_context(|| format!("creating {}", stdout.display()))?;
    let stderr_file =
        File::create(stderr).with_context(|| format!("creating {}", stderr.display()))?;
    let mut command = Command::new(&binary.executable);
    command
        .arg(command_name)
        .arg(project)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env(PHASE_TIMINGS_ENV, "1")
        .env_remove("CODEGRAPH_DIR")
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file);
    match threads.count() {
        Some(count) => command.env(THREAD_ENV, count.to_string()),
        None => command.env_remove(THREAD_ENV),
    };
    Ok(command)
}

/// An untimed `<binary> <command> <project>`: its stdout, once it exited 0.
fn run_untimed(
    binary: &BinaryInfo,
    command_name: &str,
    project: &Path,
    threads: Threads,
    dir: &Path,
    timeout: Duration,
) -> Result<String> {
    let stdout = dir.join(format!("untimed-{command_name}.stdout"));
    let stderr = dir.join(format!("untimed-{command_name}.stderr"));
    let mut command = codegraph_command(
        binary,
        command_name,
        project,
        threads,
        dir,
        &stdout,
        &stderr,
    )?;
    let measured = spawn_measured(&mut command, timeout)
        .with_context(|| format!("running {} {command_name}", binary.path))?;
    check_exit(binary, command_name, &measured, &stderr, timeout)?;
    let printed =
        fs::read_to_string(&stdout).with_context(|| format!("reading {}", stdout.display()))?;
    for path in [&stdout, &stderr] {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(printed)
}

fn check_exit(
    binary: &BinaryInfo,
    command_name: &str,
    measured: &Measured,
    stderr: &Path,
    timeout: Duration,
) -> Result<()> {
    match measured.exit {
        Exit::Code(0) => Ok(()),
        Exit::Code(code) => bail!(
            "`{} {command_name}` exited with status {code}{}",
            binary.path,
            stderr_tail(stderr)
        ),
        #[cfg(unix)]
        Exit::Signal(signal) => bail!(
            "`{} {command_name}` was killed by signal {signal}{}",
            binary.path,
            stderr_tail(stderr)
        ),
        Exit::TimedOut => bail!(
            "`{} {command_name}` exceeded the {} s run timeout and was killed{}",
            binary.path,
            timeout.as_secs(),
            stderr_tail(stderr)
        ),
        Exit::Unknown => bail!(
            "`{} {command_name}` ended without an exit status",
            binary.path
        ),
    }
}

/// The files `codegraph sync` printed it re-indexed: `Synced: <n> reindexed, …`.
pub fn reindexed_files(stdout: &str) -> Option<usize> {
    stdout.lines().find_map(|line| {
        let (count, _) = line.strip_prefix("Synced: ")?.split_once(" reindexed")?;
        count.replace(',', "").parse().ok()
    })
}

/// A sync must re-index exactly the files the run edited, or it timed
/// something else.
fn check_reindexed(binary: &BinaryInfo, stdout: &str, edited: usize) -> Result<()> {
    match reindexed_files(stdout) {
        Some(count) if count == edited => Ok(()),
        Some(count) => bail!(
            "`{} sync` re-indexed {count} files after {edited} were edited",
            binary.path
        ),
        None => bail!(
            "`{} sync` printed no `Synced: <n> reindexed` line:\n{}",
            binary.path,
            stdout.trim_end()
        ),
    }
}

fn read_phases(stderr: &Path, command_name: &str) -> Result<BTreeMap<String, f64>> {
    let text =
        fs::read_to_string(stderr).with_context(|| format!("reading {}", stderr.display()))?;
    parse_phase_lines(&text, command_name)
        .with_context(|| format!("reading the phase timings of {}", stderr.display()))
}

/// The phase-timing lines of one run's stderr, in milliseconds by phase:
/// `codegraph-phase\t<command>\t<phase>\t<microseconds>`. Other lines are
/// ignored; a tagged line that is malformed, comes from another command, or
/// repeats a phase is an error.
pub fn parse_phase_lines(stderr: &str, command_name: &str) -> Result<BTreeMap<String, f64>> {
    let mut phases = BTreeMap::new();
    for (index, line) in stderr.lines().enumerate() {
        let Some(fields) = line
            .strip_prefix(PHASE_TAG)
            .and_then(|rest| rest.strip_prefix('\t'))
        else {
            continue;
        };
        let number = index + 1;
        let fields: Vec<&str> = fields.split('\t').collect();
        let [command, phase, micros] = fields[..] else {
            bail!("line {number} is not `{PHASE_TAG}\\t<command>\\t<phase>\\t<us>`: {line:?}");
        };
        if command != command_name {
            bail!("line {number} times `{command}`, not `{command_name}`: {line:?}");
        }
        if phase.is_empty() {
            bail!("line {number} names no phase: {line:?}");
        }
        let micros: u64 = micros
            .parse()
            .with_context(|| format!("line {number} has no microsecond count: {line:?}"))?;
        if phases
            .insert(phase.to_string(), micros as f64 / 1000.0)
            .is_some()
        {
            bail!("line {number} reports {phase} a second time");
        }
    }
    Ok(phases)
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
        let measured: Vec<&RunSample> = runs
            .iter()
            .filter(|run| !run.warmup && run.void.is_none())
            .collect();
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
        let cpu: Vec<f64> = measured.iter().filter_map(|run| run.cpu_ms).collect();
        let cpu_ms = (cpu.len() == measured.len())
            .then(|| Summary::of(&cpu))
            .flatten();
        let foreign: Vec<f64> = measured
            .iter()
            .filter_map(|run| run.load.map(|load| load.foreign_cpus))
            .collect();
        let foreign_cpus = (foreign.len() == measured.len())
            .then(|| Summary::of(&foreign))
            .flatten();
        let mut phase_ms = BTreeMap::new();
        if let Some(first) = measured.first() {
            for phase in first.phase_ms.keys() {
                let values: Vec<f64> = measured
                    .iter()
                    .filter_map(|run| run.phase_ms.get(phase).copied())
                    .collect();
                if values.len() == measured.len()
                    && let Some(summary) = Summary::of(&values)
                {
                    phase_ms.insert(phase.clone(), summary);
                }
            }
        }
        let rows_per_second = rows_total
            .filter(|_| wall_ms.median > 0.0)
            .map(|rows| rows as f64 / (wall_ms.median / 1000.0));
        Ok(Self {
            runs,
            wall_ms,
            peak_rss_kb,
            rows_per_second,
            cpu_ms,
            foreign_cpus,
            phase_ms,
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
            wall_ms: ratio_of(
                Some(candidate.wall_ms.median),
                Some(baseline.wall_ms.median),
            ),
            peak_rss_kb: ratio_of(
                candidate.peak_rss_kb.map(|rss| rss.median),
                baseline.peak_rss_kb.map(|rss| rss.median),
            ),
            canonical_rows: ratio_of(
                candidate_rows.map(|rows| rows as f64),
                baseline_rows.map(|rows| rows as f64),
            ),
            rows_per_second: ratio_of(candidate.rows_per_second, baseline.rows_per_second),
        }
    }
}

fn ratio_of(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
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
    /// The child's user and system time (`wait4`); `None` off Unix.
    cpu: Option<Duration>,
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
    // The child's own high-water mark (see the module comment): read while
    // it runs, as a reaped process has no memory figures left.
    let mut polled_peak_kb: Option<u64> = None;
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
        if cfg!(target_os = "linux")
            && let Some(kb) = read_proc_status_kb(child.id(), "VmHWM:")
        {
            polled_peak_kb = polled_peak_kb.max(Some(kb));
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
    let peak_rss_kb = if cfg!(target_os = "linux") {
        polled_peak_kb
    } else {
        max_rss_kb(&usage)
    };
    Ok(Measured {
        exit,
        wall,
        peak_rss_kb,
        cpu: Some(crate::load_guard::cpu_of(&usage)),
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
        cpu: None,
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
    if cfg!(target_os = "linux") {
        "VmHWM of /proc/<pid>/status, polled every millisecond (KiB)"
    } else if cfg!(target_os = "macos") {
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
            (PHASE_TIMINGS_ENV, "1"),
            (THREAD_ENV, "<threads>"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let mut env_removed = vec!["CODEGRAPH_DIR".to_string()];
        if config.threads.contains(&Threads::Default) {
            env_removed.push(THREAD_ENV.to_string());
        }
        let invocation = match &config.op {
            AbOp::Init => "<binary> init <fresh copy of the corpus>".to_string(),
            _ => "<binary> sync <the binary's own copy of the corpus>, after a comment line is \
                  appended to each edited file; an untimed init and warm-up sync come first, \
                  and a last untimed sync is checked against a fresh init"
                .to_string(),
        };
        let arms = config.candidates.len() + 1;
        let order = if arms == 2 {
            "interleaved: even runs baseline first, odd runs candidate first".to_string()
        } else {
            format!(
                "rotated: run r starts with arm r mod {arms} (the baseline, then the candidates \
                 in order)"
            )
        };
        Self {
            invocation,
            op: config.op.label(),
            env_set,
            env_removed,
            thread_env: THREAD_ENV.to_string(),
            threads: config
                .threads
                .iter()
                .map(|threads| threads.resolved())
                .collect(),
            thread_settings: config
                .threads
                .iter()
                .map(|threads| threads.label())
                .collect(),
            runs: config.runs,
            discarded_warmup_runs: 1,
            order,
            run_timeout_secs: config.run_timeout.as_secs(),
            budget: BudgetLimits::PROJECT,
            load_guard: config.load_guard.clone(),
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

/// The candidates of a report; a schema-1 report holds only `candidate`.
fn report_candidates(report: &AbReport) -> Vec<&BinaryInfo> {
    if report.candidates.is_empty() {
        vec![&report.candidate]
    } else {
        report.candidates.iter().collect()
    }
}

/// The candidates of a cell; a schema-1 cell holds only `candidate`.
fn cell_candidates(cell: &Cell) -> Vec<CandidateCell> {
    if cell.candidates.is_empty() {
        vec![CandidateCell {
            series: cell.candidate.clone(),
            ratio: cell.ratio,
            budget: cell.budget,
            phase_ratio: BTreeMap::new(),
        }]
    } else {
        cell.candidates.clone()
    }
}

/// Render the report as Markdown: a provenance table, one row per corpus,
/// thread count and candidate, then the phases both arms of a cell report.
pub fn render_ab_markdown(report: &AbReport) -> String {
    let env = &report.environment;
    let settings = &report.settings;
    let threads = if settings.thread_settings.is_empty() {
        settings
            .threads
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
    } else {
        settings.thread_settings.clone()
    };
    let budget = settings.budget;
    let candidates = report_candidates(report);
    let mut out = String::from("# CodeGraph A/B comparison\n\n| Field | Value |\n|---|---|\n");
    let mut field = |name: &str, value: String| {
        out.push_str(&format!("| {name} | {} |\n", value.replace('|', "\\|")));
    };
    let mut binaries = vec![("Baseline".to_string(), &report.baseline)];
    for (index, candidate) in candidates.iter().enumerate() {
        binaries.push((
            title_case(&arm_name(index + 1, candidates.len())),
            candidate,
        ));
    }
    for (name, binary) in binaries {
        field(
            &name,
            format!(
                "`{}` ({})",
                binary.path,
                binary.version.as_deref().unwrap_or("version unknown")
            ),
        );
        field(&format!("{name} sha256"), format!("`{}`", binary.sha256));
    }
    field("Operation", format!("`{}`", settings.op));
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
        "Load guard",
        match (&settings.load_guard, &report.session_start) {
            (Some(guard), start) => format!(
                "on: {} quiet checks at load below {}{}; foreign CPUs ≤ {} (saturating cell) or {}",
                guard.start_checks,
                guard.start_load1_max,
                start.as_ref().map_or_else(String::new, |start| format!(
                    " ({} run, {:.0} s)",
                    start.checks.len(),
                    start.waited_secs
                )),
                guard.foreign_cpus_saturating,
                guard.foreign_cpus_partial
            ),
            (None, _) => "off".to_string(),
        },
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
            "init wall ≤ {:.2}× (or rows grew and rows/s ≥ {:.2}×); single-file sync wall ≤ \
             {:.2}×; peak RSS ≤ {:.2}×",
            budget.init_wall_ratio_max,
            budget.rows_per_second_ratio_min,
            budget.sync_wall_ratio_max,
            budget.peak_rss_ratio_max
        ),
    );

    out.push_str(
        "\n| Corpus | Threads | Candidate | Baseline ms (MAD) | Candidate ms (MAD) | Wall × | \
         Baseline RSS MiB | Candidate RSS MiB | RSS × | Rows × | Rows/s × | Graph | Budget | \
         Void runs |\n\
         |---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|---:|\n",
    );
    let mut phases = String::new();
    let mut foreign = String::new();
    for corpus in &report.corpora {
        let graphs = if corpus.graphs.is_empty() {
            vec![&corpus.graph]
        } else {
            corpus.graphs.iter().collect()
        };
        for cell in &corpus.cells {
            let threads = if cell.default_threads {
                format!("default ({})", cell.threads)
            } else {
                cell.threads.to_string()
            };
            let cell_candidates = cell_candidates(cell);
            for (index, series) in std::iter::once(&cell.baseline)
                .chain(cell_candidates.iter().map(|candidate| &candidate.series))
                .enumerate()
            {
                if let Some(row) = foreign_row(series) {
                    foreign.push_str(&format!(
                        "| {} | {threads} | {} | {row} |\n",
                        corpus.name,
                        arm_name(index, cell_candidates.len())
                    ));
                }
            }
            for (index, candidate) in cell_candidates.iter().enumerate() {
                let name = arm_name(index + 1, cell_candidates.len());
                let graph = match graphs.get(index) {
                    Some(graph) if graph.identical => "identical".to_string(),
                    Some(graph) => format!("differs: {}", graph.differing_surfaces.join(", ")),
                    None => "n/a".to_string(),
                };
                out.push_str(&format!(
                    "| {} | {threads} | {name} | {} | {} | {} | {} | {} | {} | {} | {} | {graph} \
                     | {} | {}/{} |\n",
                    corpus.name,
                    summary_ms(&cell.baseline.wall_ms),
                    summary_ms(&candidate.series.wall_ms),
                    ratio_text(candidate.ratio.wall_ms),
                    rss_mib(cell.baseline.peak_rss_kb),
                    rss_mib(candidate.series.peak_rss_kb),
                    ratio_text(candidate.ratio.peak_rss_kb),
                    ratio_text(candidate.ratio.canonical_rows),
                    ratio_text(candidate.ratio.rows_per_second),
                    budget_text(&candidate.budget),
                    void_runs(&cell.baseline),
                    void_runs(&candidate.series),
                ));
                for (phase, base) in &cell.baseline.phase_ms {
                    let Some(timed) = candidate.series.phase_ms.get(phase) else {
                        continue;
                    };
                    phases.push_str(&format!(
                        "| {} | {threads} | {name} | {phase} | {} | {} | {} |\n",
                        corpus.name,
                        summary_ms(base),
                        summary_ms(timed),
                        ratio_text(candidate.phase_ratio.get(phase).copied()),
                    ));
                }
            }
        }
    }
    if !phases.is_empty() {
        out.push_str(
            "\n| Corpus | Threads | Candidate | Phase | Baseline ms (MAD) | Candidate ms (MAD) | \
             × |\n|---|---:|---|---|---:|---:|---:|\n",
        );
        out.push_str(&phases);
    }
    if !foreign.is_empty() {
        out.push_str(
            "\nForeign CPUs the load guard measured, per run in run order:\n\n\
             | Corpus | Threads | Binary | Warm-up | Measured runs | Void runs |\n\
             |---|---:|---|---:|---|---|\n",
        );
        out.push_str(&foreign);
    }
    out
}

/// One binary's foreign CPUs in a cell, as the Markdown table's last three
/// columns; `None` when the guard measured none of its runs.
fn foreign_row(series: &ArmSeries) -> Option<String> {
    let values = |keep: &dyn Fn(&RunSample) -> bool| {
        let values: Vec<String> = series
            .runs
            .iter()
            .filter(|run| keep(run))
            .filter_map(|run| run.load.map(|load| format!("{:.2}", load.foreign_cpus)))
            .collect();
        if values.is_empty() {
            "—".to_string()
        } else {
            values.join(", ")
        }
    };
    series.runs.iter().any(|run| run.load.is_some()).then(|| {
        format!(
            "{} | {} | {}",
            values(&|run| run.warmup),
            values(&|run| !run.warmup && run.void.is_none()),
            values(&|run| run.void.is_some())
        )
    })
}

fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn void_runs(series: &ArmSeries) -> usize {
    series.runs.iter().filter(|run| run.void.is_some()).count()
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
    if verdict.sync_wall_ok == Some(false) {
        over.push("sync wall over");
    }
    if verdict.peak_rss_ok == Some(false) {
        over.push("peak RSS over");
    }
    if !over.is_empty() {
        return over.join(", ");
    }
    match (
        verdict.init_wall_ok.or(verdict.sync_wall_ok),
        verdict.peak_rss_ok,
    ) {
        (Some(true), Some(true)) => "ok".to_string(),
        (Some(true), None) => "ok (no RSS)".to_string(),
        (None, Some(true)) => "RSS ok (no wall budget)".to_string(),
        _ => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run's peak RSS is the child's own, not the address space it was
    /// forked from: with the harness holding 256 MiB, a `sleep` still
    /// reports a few MiB (W4d end: `ru_maxrss` read every run after the
    /// typescript-large corpus as the harness's 414 MiB).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_runs_peak_rss_is_the_childs_own_not_the_harnesss() {
        let held = vec![1u8; 256 << 20];
        let touched: u64 = held.iter().step_by(4096).map(|&b| u64::from(b)).sum();
        assert!(touched > 0);
        let mut command = Command::new("sleep");
        command.arg("0.2");
        let measured = spawn_measured(&mut command, Duration::from_secs(30)).expect("sleep runs");
        assert!(matches!(measured.exit, Exit::Code(0)));
        let peak = measured.peak_rss_kb.expect("the child's VmHWM is read");
        assert!(peak < 64 << 10, "a sleep's peak RSS is {peak} KiB");
        drop(held);
    }

    /// The child's CPU time comes from `wait4`: a busy loop's is most of its
    /// wall time, a `sleep`'s next to nothing.
    #[cfg(unix)]
    #[test]
    fn a_runs_cpu_time_is_the_childs_from_wait4() {
        let mut busy = Command::new("sh");
        busy.args(["-c", "i=0; while [ $i -lt 200000 ]; do i=$((i + 1)); done"]);
        let measured = spawn_measured(&mut busy, Duration::from_secs(60)).expect("sh runs");
        let cpu = measured.cpu.expect("wait4 reports CPU time");
        assert!(cpu > measured.wall / 4, "{cpu:?} of {:?}", measured.wall);
        let mut idle = Command::new("sleep");
        idle.arg("0.2");
        let measured = spawn_measured(&mut idle, Duration::from_secs(30)).expect("sleep runs");
        assert!(measured.cpu.expect("CPU time") < Duration::from_millis(100));
    }

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
        assert_eq!(verdict(ratios(1.0, 1.0, 1.0, None)).sync_wall_ok, None);
    }

    /// A single-file sync has the 1.15 line; a sync of several files no wall
    /// line; neither is an init.
    #[test]
    fn the_sync_budget_covers_a_single_file_sync() {
        let limits = BudgetLimits::PROJECT;
        let single =
            |wall| BudgetVerdict::evaluate_sync(&ratios(wall, 1.0, 1.0, Some(1.0)), &limits, true);
        assert_eq!(single(1.15).sync_wall_ok, Some(true));
        assert_eq!(single(1.16).sync_wall_ok, Some(false));
        assert_eq!(single(1.0).init_wall_ok, None);
        let several =
            BudgetVerdict::evaluate_sync(&ratios(2.0, 1.0, 1.0, Some(1.0)), &limits, false);
        assert_eq!(several.sync_wall_ok, None);
        assert_eq!(several.peak_rss_ok, Some(true));
        assert_eq!(budget_text(&several), "RSS ok (no wall budget)");
        assert_eq!(budget_text(&single(1.16)), "sync wall over");
    }

    fn sample(run: usize, wall_ms: f64, rss: u64) -> RunSample {
        RunSample {
            run,
            warmup: run == 1,
            wall_ms,
            peak_rss_kb: Some(rss),
            canonical_hash: Some("h".to_string()),
            cpu_ms: Some(wall_ms / 2.0),
            phase_ms: BTreeMap::from([
                ("scan".to_string(), wall_ms / 10.0),
                ("publish".to_string(), 1.0),
            ]),
            load: None,
            void: None,
        }
    }

    #[test]
    fn warm_up_runs_are_left_out_of_the_statistics() {
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
        assert_eq!(series.cpu_ms.map(|cpu| cpu.median), Some(6.0));
        assert_eq!(series.phase_ms["scan"].median, 1.2);
        assert_eq!(series.phase_ms["publish"].samples, 3);
    }

    /// A void run stays in the report and counts in no statistic.
    #[test]
    fn void_runs_are_left_out_of_the_statistics() {
        let mut void = sample(3, 500.0, 100);
        void.void = Some("0.90 foreign CPUs, over the 0.5 limit".to_string());
        let series = ArmSeries::new(
            vec![
                sample(1, 1000.0, 100),
                sample(2, 10.0, 100),
                void,
                sample(4, 12.0, 100),
            ],
            None,
        )
        .unwrap();
        assert_eq!(series.runs.len(), 4);
        assert_eq!(series.wall_ms.median, 11.0);
        assert_eq!(series.wall_ms.samples, 2);
        assert_eq!(void_runs(&series), 1);
    }

    #[test]
    fn every_run_of_a_cell_names_the_same_phases() {
        let runs = vec![sample(1, 10.0, 1), sample(2, 10.0, 1)];
        check_phase_names(&runs).unwrap();
        let mut odd = sample(3, 10.0, 1);
        odd.phase_ms.remove("publish");
        let error = check_phase_names(&[runs[0].clone(), odd]).unwrap_err();
        assert!(format!("{error:#}").contains("publish"), "{error:#}");
        // A binary that prints no phase timings reports none in every run.
        let mut bare = sample(1, 10.0, 1);
        bare.phase_ms.clear();
        check_phase_names(&[bare.clone(), bare]).unwrap();
    }

    #[test]
    fn ratios_are_absent_rather_than_infinite() {
        assert_eq!(ratio_of(Some(2.0), Some(0.0)), None);
        assert_eq!(ratio_of(None, Some(1.0)), None);
        assert_eq!(ratio_of(Some(3.0), Some(2.0)), Some(1.5));
    }

    #[test]
    fn runs_alternate_which_binary_goes_first() {
        assert_eq!(rotation(0, 2), [0, 1]);
        assert_eq!(rotation(1, 2), [1, 0]);
        assert_eq!(rotation(4, 2), [0, 1]);
    }

    /// With several candidates each arm goes first once in every `n` runs and
    /// keeps its place in the cycle.
    #[test]
    fn runs_rotate_through_every_arm() {
        let orders: Vec<Vec<usize>> = (0..4).map(|run| rotation(run, 3)).collect();
        assert_eq!(
            orders,
            [vec![0, 1, 2], vec![1, 2, 0], vec![2, 0, 1], vec![0, 1, 2]]
        );
        assert_eq!(arm_name(0, 2), "baseline");
        assert_eq!(arm_name(1, 1), "candidate");
        assert_eq!(arm_name(2, 2), "candidate 2");
    }

    const INIT_STDERR: &str = "Scanning files…\n\
        codegraph-phase\tinit\tscan\t6196\n\
        ✓ Indexed 3 files\n\
        codegraph-phase\tinit\tparse_write\t47273\n\
        codegraph-phase\tinit\tparse_write.store_busy\t552\n\
        codegraph-phase\tinit\tsynthesis.fieldEdges\t216\n";

    #[test]
    fn phase_lines_are_read_in_milliseconds_and_other_lines_ignored() {
        let phases = parse_phase_lines(INIT_STDERR, "init").unwrap();
        assert_eq!(
            phases,
            BTreeMap::from([
                ("parse_write".to_string(), 47.273),
                ("parse_write.store_busy".to_string(), 0.552),
                ("scan".to_string(), 6.196),
                ("synthesis.fieldEdges".to_string(), 0.216),
            ])
        );
        assert!(
            parse_phase_lines("no timings here\n", "init")
                .unwrap()
                .is_empty()
        );
        // A path holding the tag is not a phase line.
        assert!(
            parse_phase_lines("Initialized in /tmp/codegraph-phase-x\n", "init")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_or_repeated_phase_line_fails() {
        for (stderr, expected) in [
            ("codegraph-phase\tinit\tscan\n", "is not"),
            ("codegraph-phase\tinit\tscan\t1\t2\n", "is not"),
            ("codegraph-phase\tinit\tscan\t1.5\n", "microsecond"),
            ("codegraph-phase\tinit\t\t1\n", "names no phase"),
            ("codegraph-phase\tsync\tsync\t1\n", "not `init`"),
            (
                "codegraph-phase\tinit\tscan\t1\ncodegraph-phase\tinit\tscan\t2\n",
                "second time",
            ),
        ] {
            let error = parse_phase_lines(stderr, "init").unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{stderr:?}: {error:#}"
            );
        }
    }

    #[test]
    fn the_sync_summary_line_gives_the_reindexed_count() {
        assert_eq!(
            reindexed_files("Synced: 1 reindexed, 2 skipped (unchanged), 0 removed in 117ms\n"),
            Some(1)
        );
        assert_eq!(
            reindexed_files("Synced: 1,204 reindexed, 0 skipped (unchanged), 0 removed in 4s\n"),
            Some(1204)
        );
        assert_eq!(reindexed_files("Resolved 3 pending references\n"), None);
    }

    #[test]
    fn ops_and_thread_cells_parse() {
        assert_eq!(AbOp::parse("init").unwrap(), AbOp::Init);
        assert_eq!(
            AbOp::parse("sync-edit:3").unwrap(),
            AbOp::SyncEdit { files: 3 }
        );
        assert_eq!(
            AbOp::parse("sync-edit:1:src/services/rename.ts").unwrap(),
            AbOp::SyncEditFile {
                path: "src/services/rename.ts".to_string()
            }
        );
        for bad in [
            "sync",
            "sync-edit:0",
            "sync-edit:x",
            "sync-edit:2:a.ts",
            "sync-edit:1:",
            "sync-edit:1:/abs.ts",
            "sync-edit:1:../up.ts",
            "sync-edit:1:a//b.ts",
        ] {
            assert!(AbOp::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            AbOp::parse("sync-edit:1:src/a.ts").unwrap().label(),
            "sync-edit:1:src/a.ts"
        );
        assert_eq!(Threads::parse("default").unwrap(), Threads::Default);
        assert_eq!(Threads::parse("4").unwrap(), Threads::Count(4));
        assert!(Threads::parse("four").is_err());
        assert_eq!(Threads::Count(4).to_string(), "RAYON_NUM_THREADS=4");
        assert_eq!(Threads::Default.to_string(), "RAYON_NUM_THREADS unset");
    }

    fn file_row(path: &str, language: &str) -> CanonicalRow {
        CanonicalRow::from([
            ("path".to_string(), serde_json::json!(path)),
            ("language".to_string(), serde_json::json!(language)),
        ])
    }

    /// The edit picks the first files in path order that take a line comment,
    /// each with its language's comment; a named file must be indexed.
    #[test]
    fn sync_edits_pick_commentable_files_in_path_order() {
        let scratch = ScratchDir::new("edit-test").unwrap();
        let project = scratch.path();
        for (name, text) in [
            ("b.py", "x = 1\n"),
            ("a.json", "{}\n"),
            ("c.ts", "let c = 1;"),
        ] {
            fs::write(project.join(name), text).unwrap();
        }
        let rows = [
            file_row("c.ts", "typescript"),
            file_row("a.json", "json"),
            file_row("b.py", "python"),
        ];
        let edits = select_edits(&rows, &AbOp::SyncEdit { files: 2 }, project).unwrap();
        let picked: Vec<(&str, &str)> = edits
            .iter()
            .map(|edit| (edit.relative.as_str(), edit.comment))
            .collect();
        assert_eq!(picked, [("b.py", "# "), ("c.ts", "// ")]);
        let error = select_edits(&rows, &AbOp::SyncEdit { files: 3 }, project)
            .err()
            .expect("two files take a comment");
        assert!(
            format!("{error:#}").contains("the corpus has 2"),
            "{error:#}"
        );
        let named = AbOp::SyncEditFile {
            path: "c.ts".to_string(),
        };
        let mut sync = SyncArm {
            dir: project.to_path_buf(),
            project: project.to_path_buf(),
            edits: select_edits(&rows, &named, project).unwrap(),
        };
        for missing in ["a.json", "d.ts"] {
            let op = AbOp::SyncEditFile {
                path: missing.to_string(),
            };
            assert!(select_edits(&rows, &op, project).is_err(), "{missing}");
        }
        sync.append("warm-up").unwrap();
        sync.append("2").unwrap();
        assert_eq!(
            fs::read_to_string(project.join("c.ts")).unwrap(),
            "let c = 1;\n// codegraph-bench sync-edit warm-up\n\n// codegraph-bench sync-edit 2\n"
        );
        sync.write_final().unwrap();
        assert_eq!(
            fs::read_to_string(project.join("c.ts")).unwrap(),
            "let c = 1;\n// codegraph-bench sync-edit final\n"
        );
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
