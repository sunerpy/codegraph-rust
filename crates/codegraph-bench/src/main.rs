use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use codegraph_bench::ab::{
    AbConfig, AbCorpus, DEFAULT_RUN_TIMEOUT, run_ab, write_ab_markdown, write_ab_report,
};
use codegraph_bench::corpus::{corpora_root, fetch_all_in, list_statuses, list_statuses_in};
use codegraph_bench::graph_diff::run_graph_diff;
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
        help = "A/B-time a cold `init` of --baseline against --candidate on --corpora"
    )]
    ab: bool,

    #[arg(long, value_name = "BIN", help = "Baseline codegraph binary for --ab")]
    baseline: Option<PathBuf>,

    #[arg(long, value_name = "BIN", help = "Candidate codegraph binary for --ab")]
    candidate: Option<PathBuf>,

    #[arg(
        long,
        value_name = "N",
        value_delimiter = ',',
        help = "RAYON_NUM_THREADS values for --ab, comma-separated; each is its own cell"
    )]
    threads: Vec<usize>,

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
    let candidate = args
        .candidate
        .clone()
        .context("--ab needs --candidate <BIN>")?;
    if args.corpora.is_empty() {
        bail!("--ab needs --corpora <NAME[,NAME...]> (or 'all')");
    }
    if args.threads.is_empty() {
        bail!("--ab needs --threads <N[,M...]>");
    }
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
        candidate,
        corpora,
        threads: args.threads.clone(),
        runs: args.runs,
        run_timeout: DEFAULT_RUN_TIMEOUT,
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
