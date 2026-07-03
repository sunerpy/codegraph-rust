//! Task 26 full benchmark pipeline: Rust `codegraph` CLI vs the upstream
//! TypeScript CLI on the three pinned corpora.
//!
//! Methodology is documented in `docs/benchmark.md`. The pipeline:
//!
//! 1. Builds a fresh per-arm working copy of each corpus subtree so the Rust and
//!    upstream implementations index byte-identical inputs.
//! 2. ASSERTS that both freshly-indexed databases expose an identical `.schema`
//!    (the control variable) BEFORE any timing run. A mismatch fails the corpus.
//! 3. Times cold-process CLI invocations with wall-clock + `/proc/<pid>/status`
//!    `VmHWM` peak-RSS polling (no criterion for Node — that would be a category
//!    error). Cold and warm series are measured and reported separately.
//! 4. Records cold index time, incremental re-index time, query latency p50/p99,
//!    peak RSS, DB size, and a Rust-only in-process parse-only sub-metric.
//!
//! The pipeline only CONSUMES the built CLIs and the public `codegraph-extract`
//! parse API; it never modifies product-crate logic.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use codegraph_extract::{ExtractOptions, extract_project};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::corpus::{CORPORA, Corpus, corpus_benchmark_path};
use crate::metrics::{LatencyStats, db_file_size_bytes, stats};
use crate::runner::cold_mode_supported;

/// Implementations compared by the pipeline.
pub const RUST_IMPL: &str = "rust";
pub const UPSTREAM_IMPL: &str = "upstream";

/// A representative query per corpus, chosen to resolve against real symbols in
/// the indexed subtree. Query latency is the cold-process CLI cost of answering
/// these, so the symbol must exist for a meaningful (non-empty) measurement.
fn corpus_query(name: &str) -> &'static str {
    match name {
        "fd-small" => "Walk",
        "tokio-medium" => "Runtime",
        "typescript-large" => "Parser",
        _ => "main",
    }
}

/// One latency metric (median + MAD + p50/p99) plus its raw samples and the RSS
/// samples gathered alongside.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub samples_ms: Vec<f64>,
    pub median_ms: f64,
    pub mad_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub peak_rss_kb: Vec<Option<u64>>,
    pub median_peak_rss_kb: Option<u64>,
    pub raw_runs: usize,
    pub discarded_first: bool,
}

impl Measurement {
    fn from_samples(samples_ms: Vec<f64>, peak_rss_kb: Vec<Option<u64>>, raw_runs: usize) -> Self {
        let LatencyStats {
            median,
            mad,
            p50,
            p99,
        } = stats(&samples_ms).unwrap_or(LatencyStats {
            median: f64::NAN,
            mad: f64::NAN,
            p50: f64::NAN,
            p99: f64::NAN,
        });
        let median_peak_rss_kb = median_optional_u64(&peak_rss_kb);
        Self {
            samples_ms,
            median_ms: median,
            mad_ms: mad,
            p50_ms: p50,
            p99_ms: p99,
            peak_rss_kb,
            median_peak_rss_kb,
            raw_runs,
            discarded_first: true,
        }
    }
}

/// All metrics for one implementation arm on one corpus.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArmResult {
    pub command_cold_index: String,
    pub command_incremental: String,
    pub command_query: String,
    pub cold_index: Measurement,
    pub warm_index: Measurement,
    pub incremental: Measurement,
    pub query_latency_cold: Measurement,
    pub query_latency_warm: Measurement,
    pub db_size_bytes: u64,
    /// Rust-only in-process parse (extraction without resolution/DB write).
    /// `None` for the upstream arm — its CLI exposes no comparable parse-only
    /// entry point, so cross-impl parse-only is intentionally not fabricated.
    pub parse_only: Option<Measurement>,
}

/// Per-corpus result with the schema-equality control flag.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorpusBenchmark {
    pub corpus: String,
    pub commit: String,
    pub files: usize,
    pub schema_equal: bool,
    pub schema_diff: Option<String>,
    pub rust: ArmResult,
    pub upstream: ArmResult,
}

/// Pipeline-level metadata shared by all corpora.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PipelineMeta {
    pub runs: usize,
    pub discard_first: bool,
    pub cold_cache_dropped: bool,
    pub cold_cache_evicted_userspace: bool,
    pub query_per_corpus: Vec<(String, String)>,
    pub upstream_invocation: String,
}

pub struct PipelineConfig {
    pub runs: usize,
    pub corpora: Vec<Corpus>,
    pub workspace_root: PathBuf,
}

/// Run the full pipeline across the selected corpora.
pub fn run_pipeline(config: &PipelineConfig) -> Result<(PipelineMeta, Vec<CorpusBenchmark>)> {
    if config.runs < 2 {
        bail!("benchmark requires at least 2 runs (run 1 is discarded)");
    }
    let cold_cache_dropped = cold_mode_supported();
    let rust_cli = release_cli(&config.workspace_root)?;
    let upstream_cli = upstream_cli(&config.workspace_root)?;

    let mut results = Vec::new();
    for corpus in &config.corpora {
        eprintln!("==> corpus: {}", corpus.name);
        let bench = run_corpus(config, corpus, &rust_cli, &upstream_cli)?;
        results.push(bench);
    }

    let meta = PipelineMeta {
        runs: config.runs,
        discard_first: true,
        cold_cache_dropped,
        cold_cache_evicted_userspace: !cold_cache_dropped,
        query_per_corpus: config
            .corpora
            .iter()
            .map(|c| (c.name.to_string(), corpus_query(c.name).to_string()))
            .collect(),
        upstream_invocation: upstream_command_prefix(&upstream_cli),
    };
    Ok((meta, results))
}

fn run_corpus(
    config: &PipelineConfig,
    corpus: &Corpus,
    rust_cli: &Path,
    upstream_cli: &Path,
) -> Result<CorpusBenchmark> {
    let source = corpus_benchmark_path(&config.workspace_root, *corpus);
    if !source.is_dir() {
        bail!(
            "corpus {} not fetched at {} (run --fetch-corpora)",
            corpus.name,
            source.display()
        );
    }

    let scratch = config
        .workspace_root
        .join("target/bench-work")
        .join(corpus.name);
    reset_dir(&scratch)?;
    let rust_arm = scratch.join("rust");
    let upstream_arm = scratch.join("upstream");
    copy_tree(&source, &rust_arm)?;
    copy_tree(&source, &upstream_arm)?;

    // --- Schema-equality assertion (control variable) BEFORE timing. ---
    eprintln!("    asserting .schema equality ...");
    rust_index(rust_cli, &rust_arm)?;
    upstream_index(upstream_cli, &upstream_arm)?;
    let rust_schema = sqlite_schema(&db_path(&rust_arm))?;
    let upstream_schema = sqlite_schema(&db_path(&upstream_arm))?;
    let schema_equal = rust_schema == upstream_schema;
    let schema_diff = if schema_equal {
        None
    } else {
        Some(schema_text_diff(&rust_schema, &upstream_schema))
    };
    if !schema_equal {
        bail!(
            "SCHEMA MISMATCH on {} — control variable violated; refusing to time a non-identical schema.\n{}",
            corpus.name,
            schema_diff.as_deref().unwrap_or("")
        );
    }
    eprintln!("    .schema IDENTICAL — control variable holds");

    let query = corpus_query(corpus.name);
    // Per-run wall-clock cap, scaled by corpus size: a stalled upstream process
    // is killed and the run fails loudly rather than hanging the whole benchmark.
    let files = count_files(&source);
    let timeout = per_run_timeout(files);
    let rust = bench_arm(config.runs, rust_cli, &rust_arm, query, Impl::Rust, timeout)?;
    let upstream = bench_arm(
        config.runs,
        upstream_cli,
        &upstream_arm,
        query,
        Impl::Upstream,
        timeout,
    )?;

    // Free disk early — corpora copies are large.
    let _ = fs::remove_dir_all(&scratch);

    Ok(CorpusBenchmark {
        corpus: corpus.name.to_string(),
        commit: corpus.commit.to_string(),
        files,
        schema_equal,
        schema_diff,
        rust,
        upstream,
    })
}

#[derive(Clone, Copy)]
enum Impl {
    Rust,
    Upstream,
}

fn bench_arm(
    runs: usize,
    cli: &Path,
    arm: &Path,
    query: &str,
    which: Impl,
    timeout: Duration,
) -> Result<ArmResult> {
    let cold_index_cmd = cold_index_command(cli, arm, which);
    let rebuild_cmd = rebuild_command(cli, arm, which);
    let incremental_cmd = sync_command(cli, arm, which);
    let query_cmd = query_command(cli, arm, query, which);

    // COLD index: clear the index before each timed run so we measure a full
    // from-scratch build (cold index state), discarding run 1.
    eprintln!("    [{}] cold index x{runs} ...", impl_label(which));
    let (cold_ms, cold_rss) = time_series(runs, &cold_index_cmd, arm, timeout, clear_index)?;
    let cold_index = Measurement::from_samples(cold_ms, cold_rss, runs);

    // WARM index: leave the index in place and time repeated full re-index over
    // the existing DB (no pre-clear), reusing OS + SQLite caches.
    run_command_timeout(&cold_index_cmd, arm, timeout)?;
    eprintln!("    [{}] warm index x{runs} ...", impl_label(which));
    let (warm_ms, warm_rss) = time_series(runs, &rebuild_cmd, arm, timeout, |_| Ok(()))?;
    let warm_index = Measurement::from_samples(warm_ms, warm_rss, runs);

    // INCREMENTAL re-index: touch one source file, then sync. Each timed run
    // re-touches a file so the sync has a real (single-file) delta to absorb.
    eprintln!("    [{}] incremental x{runs} ...", impl_label(which));
    let (inc_ms, inc_rss) = time_series(runs, &incremental_cmd, arm, timeout, touch_one_source)?;
    let incremental = Measurement::from_samples(inc_ms, inc_rss, runs);

    // QUERY latency: ensure a fully-built index, then time the query CLI.
    run_command_timeout(&rebuild_cmd, arm, timeout)?;
    eprintln!("    [{}] query (cold-proc) x{runs} ...", impl_label(which));
    let (qc_ms, qc_rss) = time_series(runs, &query_cmd, arm, timeout, |_| Ok(()))?;
    let query_latency_cold = Measurement::from_samples(qc_ms, qc_rss, runs);
    // A second pass after the first has warmed caches.
    let (qw_ms, qw_rss) = time_series(runs, &query_cmd, arm, timeout, |_| Ok(()))?;
    let query_latency_warm = Measurement::from_samples(qw_ms, qw_rss, runs);

    let db_size_bytes = db_file_size_bytes(&db_path(arm)).unwrap_or(0);

    let parse_only = match which {
        Impl::Rust => Some(rust_parse_only(runs, arm)?),
        Impl::Upstream => None,
    };

    Ok(ArmResult {
        command_cold_index: cold_index_cmd,
        command_incremental: incremental_cmd,
        command_query: query_cmd,
        cold_index,
        warm_index,
        incremental,
        query_latency_cold,
        query_latency_warm,
        db_size_bytes,
        parse_only,
    })
}

/// Rust-only parse-only sub-metric: in-process extraction (no resolution, no DB
/// write) via the public `codegraph-extract` API. Documented as Rust-only.
fn rust_parse_only(runs: usize, arm: &Path) -> Result<Measurement> {
    let options = ExtractOptions::default();
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        let _ = extract_project(arm, &options).context("parse-only extract_project")?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    // Parse-only has no separate process, so RSS is not collected per run.
    let rss = vec![None; samples.len()];
    Ok(Measurement::from_samples(samples, rss, runs))
}

/// Time a command `runs` times, running `prepare(arm)` before each run, and
/// discarding the first run. Returns (durations_ms, peak_rss_kb).
fn time_series<F>(
    runs: usize,
    command: &str,
    arm: &Path,
    timeout: Duration,
    mut prepare: F,
) -> Result<(Vec<f64>, Vec<Option<u64>>)>
where
    F: FnMut(&Path) -> Result<()>,
{
    let mut ms = Vec::with_capacity(runs);
    let mut rss = Vec::with_capacity(runs);
    for _ in 0..runs {
        prepare(arm)?;
        let measurement =
            crate::metrics::run_with_proc_status_timeout(command, Some(arm), Some(timeout))?;
        if measurement.status_code != Some(0) {
            bail!(
                "command failed ({:?}): {}\nstderr:\n{}",
                measurement.status_code,
                command,
                measurement.stderr
            );
        }
        ms.push(measurement.duration_ms);
        rss.push(measurement.peak_rss_kb);
    }
    // Discard run 1.
    let start = usize::from(ms.len() > 1);
    Ok((ms[start..].to_vec(), rss[start..].to_vec()))
}

fn run_command(command: &str, arm: &Path) -> Result<()> {
    run_command_timeout(command, arm, Duration::from_secs(900))
}

fn run_command_timeout(command: &str, arm: &Path, timeout: Duration) -> Result<()> {
    let measurement =
        crate::metrics::run_with_proc_status_timeout(command, Some(arm), Some(timeout))?;
    if measurement.status_code != Some(0) {
        bail!(
            "setup command failed ({:?}): {}\nstderr:\n{}",
            measurement.status_code,
            command,
            measurement.stderr
        );
    }
    Ok(())
}

// --- Command builders ---------------------------------------------------------

fn impl_label(which: Impl) -> &'static str {
    match which {
        Impl::Rust => RUST_IMPL,
        Impl::Upstream => UPSTREAM_IMPL,
    }
}

/// From-scratch index of a freshly-cleared project. Neither CLI's `index`
/// subcommand creates `.codegraph/` on an uninitialized project, so the cold
/// build uses `init` for both: Rust `init` indexes in one step, the upstream
/// `init -i` (the `-i`/`--index` flag) builds the initial graph. The cold
/// prepare hook clears the index dir first so each timed run is a true
/// from-scratch build.
fn cold_index_command(cli: &Path, arm: &Path, which: Impl) -> String {
    match which {
        Impl::Rust => format!("{} init {}", shell_quote(cli), shell_quote(arm)),
        Impl::Upstream => format!(
            "{} init -i {}",
            upstream_command_prefix(cli),
            shell_quote(arm)
        ),
    }
}

/// Full re-index over an already-initialized project (used for the warm series
/// and to (re)build the index before query timing). Both CLIs use a forced
/// re-index here.
fn rebuild_command(cli: &Path, arm: &Path, which: Impl) -> String {
    match which {
        Impl::Rust => format!(
            "{} index {} --force --quiet",
            shell_quote(cli),
            shell_quote(arm)
        ),
        Impl::Upstream => format!(
            "{} index {} --force --quiet",
            upstream_command_prefix(cli),
            shell_quote(arm)
        ),
    }
}

fn sync_command(cli: &Path, arm: &Path, which: Impl) -> String {
    match which {
        Impl::Rust => format!("{} sync {} --quiet", shell_quote(cli), shell_quote(arm)),
        Impl::Upstream => format!(
            "{} sync {} --quiet",
            upstream_command_prefix(cli),
            shell_quote(arm)
        ),
    }
}

fn query_command(cli: &Path, arm: &Path, query: &str, which: Impl) -> String {
    match which {
        Impl::Rust => format!(
            "{} query {} -p {} --json --limit 10",
            shell_quote(cli),
            shell_quote(Path::new(query)),
            shell_quote(arm)
        ),
        Impl::Upstream => format!(
            "{} query {} -p {} --json --limit 10",
            upstream_command_prefix(cli),
            shell_quote(Path::new(query)),
            shell_quote(arm)
        ),
    }
}

/// The upstream CLI requires the documented Node-25 unsafe override and runs
/// cleanly (single process exit) with the daemon and watcher disabled. The
/// runner wraps commands in `sh -c "exec ..."`, so `env` is used to set the
/// inline variables rather than a `VAR=val` prefix (which `exec` would treat as
/// a program name).
fn upstream_command_prefix(upstream_cli: &Path) -> String {
    format!(
        "env CODEGRAPH_ALLOW_UNSAFE_NODE=1 CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 node {}",
        shell_quote(upstream_cli)
    )
}

// --- Preparation hooks --------------------------------------------------------

fn clear_index(arm: &Path) -> Result<()> {
    let dir = arm.join(".codegraph");
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
    }
    Ok(())
}

/// Touch exactly one source file (append + remove a no-op trailing newline) so
/// the next sync sees a single-file content delta. Picks the lexicographically
/// first source file deterministically.
fn touch_one_source(arm: &Path) -> Result<()> {
    let options = ExtractOptions::default();
    let files = codegraph_extract::engine::scan_project(arm, &options)?;
    let Some(first) = files.first() else {
        bail!("no source files to touch in {}", arm.display());
    };
    let path = arm.join(first);
    let mut content = fs::read_to_string(&path)
        .with_context(|| format!("reading {} for touch", path.display()))?;
    // Append then immediately rewrite WITH a trailing marker comment line that
    // changes content (so the hash gate fires) but keeps the file parseable.
    if content.ends_with("\n// codegraph-bench-touch\n") {
        content = content.replace("\n// codegraph-bench-touch\n", "\n");
    } else {
        content.push_str("\n// codegraph-bench-touch\n");
    }
    fs::write(&path, content).with_context(|| format!("touching {}", path.display()))?;
    Ok(())
}

// --- CLI discovery ------------------------------------------------------------

fn release_cli(workspace_root: &Path) -> Result<PathBuf> {
    let candidate = workspace_root.join("target/release/codegraph");
    if !candidate.is_file() {
        bail!(
            "release CLI not found at {} — build it with `cargo build --release -p codegraph-cli`",
            candidate.display()
        );
    }
    Ok(candidate)
}

fn upstream_cli(workspace_root: &Path) -> Result<PathBuf> {
    let candidate = workspace_root.join("reference/colby/dist/bin/codegraph.js");
    if !candidate.is_file() {
        bail!(
            "upstream CLI not found at {} — build it per reference/colby/RUN.md (npm ci && npm run build)",
            candidate.display()
        );
    }
    Ok(candidate)
}

// --- helpers ------------------------------------------------------------------

fn rust_index(cli: &Path, arm: &Path) -> Result<()> {
    clear_index(arm)?;
    run_command(&cold_index_command(cli, arm, Impl::Rust), arm)
}

fn upstream_index(cli: &Path, arm: &Path) -> Result<()> {
    clear_index(arm)?;
    run_command(&cold_index_command(cli, arm, Impl::Upstream), arm)
}

fn db_path(arm: &Path) -> PathBuf {
    arm.join(".codegraph/codegraph.db")
}

fn sqlite_schema(db: &Path) -> Result<String> {
    let conn = Connection::open(db)
        .with_context(|| format!("opening {} for schema dump", db.display()))?;
    Ok(normalize_schema(&schema_dump(&conn)?))
}

// Byte-equivalent to `sqlite3 .schema`: rowid order, and the `/* name(cols) */`
// comment the shell appends after each CREATE VIRTUAL TABLE (kept so the golden
// schema string matches the old CLI path).
fn schema_dump(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT name, type, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut raw = String::new();
    for (name, kind, sql) in rows {
        raw.push_str(&sql);
        if kind == "table" && sql.starts_with("CREATE VIRTUAL TABLE") {
            raw.push_str(&format!(
                "\n/* {}({}) */",
                name,
                virtual_table_columns(conn, &name)?
            ));
        }
        raw.push_str(";\n");
    }
    Ok(raw)
}

fn virtual_table_columns(conn: &Connection, table: &str) -> Result<String> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let columns = stmt
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns.join(","))
}

/// Canonicalize a `.schema` dump for equality comparison: trim trailing
/// whitespace per line, drop blank lines, strip optional `IF NOT EXISTS`.
fn normalize_schema(raw: &str) -> String {
    let mut statements: Vec<String> = Vec::new();
    for stmt in raw.split(';') {
        let cleaned: String = stmt
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            continue;
        }
        let cleaned = cleaned
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE")
            .replace("CREATE INDEX IF NOT EXISTS", "CREATE INDEX")
            .replace("CREATE VIRTUAL TABLE IF NOT EXISTS", "CREATE VIRTUAL TABLE")
            .replace("CREATE TRIGGER IF NOT EXISTS", "CREATE TRIGGER");
        statements.push(cleaned);
    }
    let mut out = statements.join(";\n");
    out.push_str(";\n");
    out
}

fn schema_text_diff(rust: &str, upstream: &str) -> String {
    let rust_lines: Vec<&str> = rust.lines().collect();
    let upstream_lines: Vec<&str> = upstream.lines().collect();
    let mut diff = String::new();
    let max = rust_lines.len().max(upstream_lines.len());
    for i in 0..max {
        let r = rust_lines.get(i).copied().unwrap_or("<none>");
        let c = upstream_lines.get(i).copied().unwrap_or("<none>");
        if r != c {
            diff.push_str(&format!(
                "line {}:\n  rust     : {r}\n  upstream : {c}\n",
                i + 1
            ));
        }
    }
    if diff.is_empty() {
        diff.push_str("(schemas differ only in statement ordering/whitespace)");
    }
    diff
}

fn reset_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn count_files(root: &Path) -> usize {
    let options = ExtractOptions::default();
    codegraph_extract::engine::scan_project(root, &options)
        .map(|files| files.len())
        .unwrap_or(0)
}

/// Per-run wall-clock timeout, scaled by file count. A from-scratch upstream
/// index of the large corpus takes tens of seconds on Node 25; the cap is
/// generous (≈10× the observed honest runtime) so it only fires on a true
/// stall, never on a slow-but-progressing run.
fn per_run_timeout(files: usize) -> Duration {
    let secs = 120 + (files as u64) * 2;
    Duration::from_secs(secs.min(1800))
}

fn median_optional_u64(values: &[Option<u64>]) -> Option<u64> {
    let mut present: Vec<u64> = values.iter().filter_map(|v| *v).collect();
    if present.is_empty() {
        return None;
    }
    present.sort_unstable();
    let mid = present.len() / 2;
    if present.len() % 2 == 1 {
        Some(present[mid])
    } else {
        Some((present[mid - 1] + present[mid]) / 2)
    }
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Select corpora by name, or all when `names` is empty.
pub fn select_corpora(names: &[String]) -> Result<Vec<Corpus>> {
    if names.is_empty() {
        return Ok(CORPORA.to_vec());
    }
    let mut selected = Vec::new();
    for name in names {
        let found = CORPORA
            .iter()
            .copied()
            .find(|c| c.name == name)
            .with_context(|| format!("unknown corpus '{name}'"))?;
        selected.push(found);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_schema_strips_if_not_exists_and_blanks() {
        let raw = "CREATE TABLE IF NOT EXISTS nodes (id TEXT);\n\nCREATE INDEX IF NOT EXISTS idx ON nodes(id);\n";
        let got = normalize_schema(raw);
        assert!(got.contains("CREATE TABLE nodes"));
        assert!(got.contains("CREATE INDEX idx"));
        assert!(!got.contains("IF NOT EXISTS"));
        assert!(got.ends_with(";\n"));
    }

    #[test]
    fn normalize_schema_equal_for_whitespace_variants() {
        let a = "CREATE TABLE nodes (id TEXT);";
        let b = "CREATE TABLE nodes (id TEXT);\n   \n";
        assert_eq!(normalize_schema(a), normalize_schema(b));
    }

    #[test]
    fn select_corpora_all_when_empty() {
        let all = select_corpora(&[]).unwrap();
        assert_eq!(all.len(), CORPORA.len());
    }

    #[test]
    fn select_corpora_rejects_unknown() {
        assert!(select_corpora(&["does-not-exist".to_string()]).is_err());
    }

    #[test]
    fn select_corpora_picks_named() {
        let picked = select_corpora(&["fd-small".to_string()]).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "fd-small");
    }

    #[test]
    fn median_optional_handles_none_and_values() {
        assert_eq!(median_optional_u64(&[None, None]), None);
        assert_eq!(
            median_optional_u64(&[Some(10), Some(30), Some(20)]),
            Some(20)
        );
        assert_eq!(median_optional_u64(&[Some(10), Some(20)]), Some(15));
    }
}
