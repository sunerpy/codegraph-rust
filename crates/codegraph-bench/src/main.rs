use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use codegraph_bench::ab::{
    AbConfig, AbCorpus, AbOp, DEFAULT_RUN_TIMEOUT, Threads, logical_cpus, run_ab,
    write_ab_markdown, write_ab_report,
};
use codegraph_bench::corpus::{corpora_root, fetch_all_in, list_statuses, list_statuses_in};
use codegraph_bench::go_std_gen;
use codegraph_bench::graph_diff::run_graph_diff;
use codegraph_bench::load_guard::{GuardOverrides, LoadGuard};
use codegraph_bench::markdown::{render_from_json, write_markdown};
use codegraph_bench::oracle::golden::write_golden;
use codegraph_bench::pipeline::{PipelineConfig, run_pipeline, select_corpora};
use codegraph_bench::report::{
    BenchmarkReport, PipelineReport, write_pipeline_report, write_report,
};
use codegraph_bench::runner::{CacheMode, RunConfig, run_command};

#[derive(Debug, Parser)]
#[command(author, version, about = "CodeGraph benchmark harness")]
struct Args {
    #[arg(
        long,
        help = "Print the pinned corpus registry and live counts for fetched corpora"
    )]
    list_corpora: bool,

    #[arg(
        long,
        help = "Fetch pinned corpora into bench/corpora/<name> (or <--corpora-root>/<name>)"
    )]
    fetch_corpora: bool,

    #[arg(
        long,
        help = "Run the full task-26 Rust-vs-upstream benchmark pipeline on the selected corpora"
    )]
    run: bool,

    #[arg(
        long,
        value_name = "NAME",
        value_delimiter = ',',
        help = "Corpus to benchmark: repeat the flag or pass a comma-separated list. \
                For --run, omit it or pass 'all' for every pinned corpus; --ab needs a list or 'all'."
    )]
    corpora: Vec<String>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Directory holding the corpus checkouts for --ab, --list-corpora and \
                --fetch-corpora (default: bench/corpora in the workspace)"
    )]
    corpora_root: Option<PathBuf>,

    #[arg(
        long,
        help = "A/B-time --op (a cold `init` by default) of --baseline against each --candidate \
                on --corpora"
    )]
    ab: bool,

    #[arg(long, value_name = "BIN", help = "Baseline codegraph binary for --ab")]
    baseline: Option<PathBuf>,

    #[arg(
        long,
        value_name = "BIN",
        help = "Candidate codegraph binary for --ab; repeat it to time several candidates in \
                one rotation, each compared with the baseline"
    )]
    candidate: Vec<PathBuf>,

    #[arg(
        long,
        value_name = "N",
        value_delimiter = ',',
        help = "RAYON_NUM_THREADS values for --ab, comma-separated; each is its own cell. \
                `default` is a cell with the variable unset, and the default when omitted"
    )]
    threads: Vec<String>,

    #[arg(
        long,
        value_name = "OP",
        default_value = "init",
        help = "What --ab times: `init`, `sync-edit:<N>` (a sync after a comment line is \
                appended to the first N indexed files that take one) or `sync-edit:1:<path>`"
    )]
    op: String,

    #[arg(
        long,
        help = "Time --ab without the load guard (no quiet start, no waits, no void runs)"
    )]
    no_load_guard: bool,

    #[arg(
        long,
        value_name = "CPUS",
        conflicts_with = "no_load_guard",
        help = "The foreign CPUs a run may share its time with when its threads cover every \
                CPU (the `default` cell included) before the guard voids it (default 0.5)"
    )]
    guard_max_foreign_saturating: Option<f64>,

    #[arg(
        long,
        value_name = "CPUS",
        conflicts_with = "no_load_guard",
        help = "The foreign CPUs a run of fewer threads than CPUs may share its time with \
                before the guard voids it (default 2)"
    )]
    guard_max_foreign_partial: Option<f64>,

    #[arg(
        long,
        value_name = "LOAD",
        conflicts_with = "no_load_guard",
        help = "The 1-minute load each quiet-start check must stay below (default 3)"
    )]
    guard_start_max_load: Option<f64>,

    #[arg(
        long,
        value_name = "CMD",
        help = "Smoke-test the outer runner with an arbitrary shell command"
    )]
    smoke: Option<String>,

    #[arg(
        long,
        default_value_t = 12,
        help = "Number of process runs per cell for --smoke, --run and --ab; the first is discarded"
    )]
    runs: usize,

    #[arg(long, value_enum, default_value_t = CliCacheMode::Warm, help = "Cache mode for --smoke")]
    mode: CliCacheMode,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write the JSON report for --smoke, --run or --ab (default: stdout)"
    )]
    out: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write the rendered Markdown report for --run (docs/benchmark-results.md) or --ab"
    )]
    report_md: Option<PathBuf>,

    #[arg(
        long,
        value_names = ["RESULTS_JSON", "OUT_MD"],
        num_args = 2,
        help = "Re-render docs/benchmark-results.md from an existing results.json (no re-run)"
    )]
    render_md: Option<Vec<PathBuf>>,

    #[arg(
        long,
        value_names = ["DB", "OUTDIR"],
        num_args = 2,
        help = "Generate canonical golden files from an upstream SQLite database"
    )]
    gen_golden: Option<Vec<PathBuf>>,

    #[arg(
        long,
        value_names = ["LEFT", "RIGHT"],
        num_args = 2,
        help = "Compare two canonical graphs, each a SQLite database or a golden directory; \
                exit 0 when identical, 1 when different, 2 when an input cannot be loaded"
    )]
    graph_diff: Option<Vec<PathBuf>>,

    #[arg(
        long,
        requires = "go_src",
        help = "Regenerate the Go standard-library method table from a Go source tree \
                (codegraph-resolve/src/go_std_methods.rs): print it, or with --check \
                exit 1 when the committed rows differ, or with --write rewrite them"
    )]
    gen_go_std_methods: bool,

    #[arg(
        long,
        value_name = "DIR",
        help = "Go source tree (with src/io) for --gen-go-std-methods"
    )]
    go_src: Option<PathBuf>,

    #[arg(
        long,
        requires = "gen_go_std_methods",
        conflicts_with = "write",
        help = "Compare the generated rows with the committed ones"
    )]
    check: bool,

    #[arg(
        long,
        requires = "gen_go_std_methods",
        help = "Write the generated rows into the table module"
    )]
    write: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliCacheMode {
    Warm,
    Cold,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(paths) = &args.graph_diff {
        return graph_diff_exit(&paths[0], &paths[1]);
    }
    if args.gen_go_std_methods {
        return go_std_methods_exit(&args);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// `--graph-diff` follows the `cmp`/`diff` convention: 0 identical, 1
/// different, 2 when an input cannot be loaded or the report cannot be written.
fn graph_diff_exit(left: &Path, right: &Path) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    match run_graph_diff(left, right, &mut stdout) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(2)
        }
    }
}

/// `--gen-go-std-methods`: 0 when printed, written or matching; 1 when
/// `--check` finds a difference or the table cannot be generated (a listed
/// type missing, a signature that does not read), as upstream's generator
/// fails; 2 when `--go-src` is not a Go source tree.
fn go_std_methods_exit(args: &Args) -> ExitCode {
    let mode = if args.check {
        go_std_gen::Mode::Check
    } else if args.write {
        go_std_gen::Mode::Write
    } else {
        go_std_gen::Mode::Print
    };
    let go_src = args.go_src.as_deref().expect("clap requires --go-src");
    if !go_src.join("src").join("io").is_dir() {
        eprintln!(
            "Error: {} is not a Go source tree (no src/io)",
            go_src.display()
        );
        return ExitCode::from(2);
    }
    let mut stdout = std::io::stdout().lock();
    match go_std_gen::run(go_src, &go_std_gen::table_path(), mode, &mut stdout) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &Args) -> Result<()> {
    if args.fetch_corpora {
        let statuses = fetch_all_in(&selected_corpora_root(args)?)?;
        print_statuses(&statuses);
    }

    if args.list_corpora {
        let statuses = list_statuses_in(&selected_corpora_root(args)?);
        print_statuses(&statuses);
    }

    if let Some(paths) = &args.render_md {
        render_from_json(&paths[0], &paths[1])?;
        eprintln!("wrote {}", paths[1].display());
        return Ok(());
    }

    if args.ab {
        return run_ab_command(args);
    }

    if args.run {
        let workspace_root = workspace_root()?;
        let names: Vec<String> = args
            .corpora
            .iter()
            .filter(|n| n.as_str() != "all")
            .cloned()
            .collect();
        let corpora = select_corpora(&names)?;
        let config = PipelineConfig {
            runs: args.runs,
            corpora,
            workspace_root: workspace_root.clone(),
        };
        let (meta, results) = run_pipeline(&config)?;
        let report = PipelineReport::new(&workspace_root, meta, results);
        match &args.out {
            Some(path) => {
                write_pipeline_report(path, &report)?;
                eprintln!("wrote {}", path.display());
            }
            None => println!("{}", serde_json::to_string_pretty(&report)?),
        }
        if let Some(md_path) = &args.report_md {
            write_markdown(md_path, &report)?;
            eprintln!("wrote {}", md_path.display());
        }
        return Ok(());
    }

    if let Some(paths) = &args.gen_golden {
        write_golden(&paths[0], &paths[1])?;
    }

    if let Some(command) = &args.smoke {
        let workspace_root = workspace_root()?;
        let summary = run_command(
            &RunConfig {
                command: command.clone(),
                runs: args.runs,
                discard_first: true,
                mode: args.mode.into(),
            },
            Some(&workspace_root),
        )?;
        let report =
            BenchmarkReport::smoke(&workspace_root, summary, list_statuses(&workspace_root));
        match &args.out {
            Some(path) => write_report(path, &report)?,
            None => println!("{}", serde_json::to_string_pretty(&report)?),
        }
    }

    Ok(())
}

fn run_ab_command(args: &Args) -> Result<()> {
    let baseline = args
        .baseline
        .clone()
        .context("--ab needs --baseline <BIN>")?;
    if args.candidate.is_empty() {
        bail!("--ab needs --candidate <BIN>");
    }
    if args.corpora.is_empty() {
        bail!("--ab needs --corpora <NAME[,NAME...]> (or 'all')");
    }
    let threads = if args.threads.is_empty() {
        vec![Threads::Default]
    } else {
        args.threads
            .iter()
            .map(|text| Threads::parse(text))
            .collect::<Result<Vec<_>>>()?
    };
    let op = AbOp::parse(&args.op)?;
    let load_guard = load_guard_of(args, Path::new("/proc/stat").is_file(), logical_cpus())?;
    let names: Vec<String> = args
        .corpora
        .iter()
        .filter(|name| name.as_str() != "all")
        .cloned()
        .collect();
    let root = selected_corpora_root(args)?;
    let corpora = select_corpora(&names)?
        .into_iter()
        .map(|corpus| AbCorpus::from_registry(corpus, &root))
        .collect::<Result<Vec<_>>>()?;
    let config = AbConfig {
        baseline,
        candidates: args.candidate.clone(),
        corpora,
        threads,
        runs: args.runs,
        run_timeout: DEFAULT_RUN_TIMEOUT,
        op,
        load_guard,
        workspace_root: workspace_root().ok(),
        argv: std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    };
    let report = run_ab(&config)?;
    match &args.out {
        Some(path) => {
            write_ab_report(path, &report)?;
            eprintln!("wrote {}", path.display());
        }
        None => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    if let Some(path) = &args.report_md {
        write_ab_markdown(path, &report)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

/// The load guard of an `--ab` run: the standard limits for `cpus` CPUs with
/// the `--guard-*` overrides, or none with `--no-load-guard` or on a host
/// without `/proc/stat` (`proc_stat`), where it cannot read the load.
fn load_guard_of(args: &Args, proc_stat: bool, cpus: usize) -> Result<Option<LoadGuard>> {
    if args.no_load_guard || !proc_stat {
        return Ok(None);
    }
    let overrides = GuardOverrides {
        foreign_cpus_saturating: args.guard_max_foreign_saturating,
        foreign_cpus_partial: args.guard_max_foreign_partial,
        start_load1_max: args.guard_start_max_load,
    };
    LoadGuard::standard(cpus).overridden(&overrides).map(Some)
}

fn selected_corpora_root(args: &Args) -> Result<PathBuf> {
    match &args.corpora_root {
        Some(root) => Ok(root.clone()),
        None => Ok(corpora_root(&workspace_root()?)),
    }
}

impl From<CliCacheMode> for CacheMode {
    fn from(value: CliCacheMode) -> Self {
        match value {
            CliCacheMode::Warm => CacheMode::Warm,
            CliCacheMode::Cold => CacheMode::Cold,
        }
    }
}

fn print_statuses(statuses: &[codegraph_bench::corpus::CorpusStatus]) {
    println!(
        "name\tcommit\texpected_loc\texpected_files\tactual_loc\tactual_files\tfetched\tpath\ttag"
    );
    for status in statuses {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            status.name,
            status.commit,
            status.expected_loc,
            status.expected_files,
            status
                .actual_loc
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            status
                .actual_files
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            status.fetched,
            status.path,
            status.tag.as_deref().unwrap_or("-")
        );
    }
}

fn workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("crates/codegraph-bench").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("cannot locate workspace root from current directory");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(args: &[&str]) -> Result<Option<LoadGuard>> {
        let args = Args::try_parse_from(std::iter::once("bench").chain(args.iter().copied()))?;
        load_guard_of(&args, true, 32)
    }

    /// A `--guard-*` flag replaces its default and leaves the others.
    #[test]
    fn a_guard_flag_overrides_its_default() {
        let standard = LoadGuard::standard(32);
        assert_eq!(guard(&["--ab"]).unwrap(), Some(standard.clone()));
        let tuned = guard(&[
            "--ab",
            "--guard-max-foreign-partial",
            "3.5",
            "--guard-start-max-load",
            "6",
        ])
        .unwrap()
        .expect("the guard is on");
        assert_eq!(tuned.foreign_cpus_partial, 3.5);
        assert_eq!(tuned.start_load1_max, 6.0);
        assert_eq!(
            tuned.foreign_cpus_saturating,
            standard.foreign_cpus_saturating
        );
        let saturating = guard(&["--ab", "--guard-max-foreign-saturating", "1.25"])
            .unwrap()
            .expect("the guard is on");
        assert_eq!(saturating.foreign_cpus_saturating, 1.25);
        assert_eq!(
            saturating.foreign_cpus_partial,
            standard.foreign_cpus_partial
        );
        assert!(guard(&["--ab", "--guard-max-foreign-partial", "-1"]).is_err());
        assert!(guard(&["--ab", "--no-load-guard", "--guard-start-max-load", "4"]).is_err());
        assert_eq!(guard(&["--ab", "--no-load-guard"]).unwrap(), None);
        let args = Args::try_parse_from(["bench", "--ab"]).unwrap();
        assert_eq!(
            load_guard_of(&args, false, 32).unwrap(),
            None,
            "no /proc/stat"
        );
    }
}
