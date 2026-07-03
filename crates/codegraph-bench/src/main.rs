use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use codegraph_bench::corpus::{fetch_all, list_statuses};
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

    #[arg(long, help = "Fetch pinned corpora into bench/corpora/<name>")]
    fetch_corpora: bool,

    #[arg(
        long,
        help = "Run the full task-26 Rust-vs-upstream benchmark pipeline on the selected corpora"
    )]
    run: bool,

    #[arg(
        long,
        value_name = "NAME",
        help = "Corpus to benchmark (repeatable). Omit or pass 'all' for every pinned corpus."
    )]
    corpora: Vec<String>,

    #[arg(
        long,
        value_name = "CMD",
        help = "Smoke-test the outer runner with an arbitrary shell command"
    )]
    smoke: Option<String>,

    #[arg(
        long,
        default_value_t = 12,
        help = "Number of process runs for --smoke"
    )]
    runs: usize,

    #[arg(long, value_enum, default_value_t = CliCacheMode::Warm, help = "Cache mode for --smoke")]
    mode: CliCacheMode,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write a results.json report for --smoke or --run"
    )]
    out: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write the rendered markdown report (docs/benchmark-results.md) for --run"
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
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliCacheMode {
    Warm,
    Cold,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let workspace_root = workspace_root()?;

    if args.fetch_corpora {
        let statuses = fetch_all(&workspace_root)?;
        print_statuses(&statuses);
    }

    if args.list_corpora {
        let statuses = list_statuses(&workspace_root);
        print_statuses(&statuses);
    }

    if let Some(paths) = &args.render_md {
        render_from_json(&paths[0], &paths[1])?;
        eprintln!("wrote {}", paths[1].display());
        return Ok(());
    }

    if args.run {
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

    if let Some(paths) = args.gen_golden {
        write_golden(&paths[0], &paths[1])?;
    }

    if let Some(command) = args.smoke {
        let summary = run_command(
            &RunConfig {
                command,
                runs: args.runs,
                discard_first: true,
                mode: args.mode.into(),
            },
            Some(&workspace_root),
        )?;
        let report =
            BenchmarkReport::smoke(&workspace_root, summary, list_statuses(&workspace_root));
        match args.out {
            Some(path) => write_report(&path, &report)?,
            None => println!("{}", serde_json::to_string_pretty(&report)?),
        }
    }

    Ok(())
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
    println!("name\tcommit\texpected_loc\texpected_files\tactual_loc\tactual_files\tfetched\tpath");
    for status in statuses {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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
            status.path
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
