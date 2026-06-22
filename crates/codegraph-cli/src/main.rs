//! Single `codegraph` CLI binary.
//!
//! This crate owns process bootstrap: load config fail-fast, initialize tracing,
//! keep the `WorkerGuard` alive, then run the requested command. Library crates
//! only emit tracing events.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use codegraph_core::config::init_config;
use codegraph_core::logger::{init_logger, LoggerConfig};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{FileRecord, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file, ExtractOptions};
use codegraph_graph::graph::GraphTraverser;
use codegraph_graph::query::{search_nodes, SearchOptions};
use codegraph_mcp::McpServer;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::queries::SearchResult;
use codegraph_store::Store;
use serde::Serialize;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

mod installer;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EXTRACTION_VERSION: i64 = 1;

fn main() {
    let cli = Cli::parse();
    let bootstrap_root = cli.bootstrap_project_root();
    let config = match init_config(None, &bootstrap_root) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("CodeGraph config error: {err:#}");
            std::process::exit(1);
        }
    };

    let logger_cfg = LoggerConfig {
        level: config.app.log_level.clone(),
        stdout: false,
        file: false,
        ..Default::default()
    };
    let _guard = match init_logger(&logger_cfg) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("CodeGraph logger error: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = run(cli) {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

#[derive(Debug, Parser)]
#[command(name = "codegraph")]
#[command(version = VERSION)]
#[command(about = "Code intelligence and knowledge graph for any codebase")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn bootstrap_project_root(&self) -> PathBuf {
        let raw = match &self.command {
            Command::Init { path }
            | Command::Uninit { path, .. }
            | Command::Index { path, .. }
            | Command::Sync { path, .. }
            | Command::Status { path, .. }
            | Command::Unlock { path } => path.clone(),
            Command::Query { path, .. }
            | Command::Files { path, .. }
            | Command::Serve { path, .. }
            | Command::Callers { path, .. }
            | Command::Callees { path, .. }
            | Command::Impact { path, .. }
            | Command::Affected { path, .. }
            | Command::Check { path, .. } => path.clone(),
            Command::Export { path, .. } => path.clone(),
            // install/uninstall are not project-scoped — bootstrap from cwd.
            Command::Install { .. }
            | Command::Uninstall { .. }
            | Command::Version
            | Command::Completions { .. }
            | Command::SelfUpdate { .. } => None,
        };
        let start = absolute_path(raw.unwrap_or_else(|| PathBuf::from(".")));
        resolve_project_path_optional(&start)
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    // Upstream flags/output: upstream bin/codegraph.ts:420-424, 431-470.
    Init {
        path: Option<PathBuf>,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:482-485, 489-527.
    Uninit {
        path: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:536-540, 545-596.
    Index {
        path: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        quiet: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:605-608, 612-657.
    Sync {
        path: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:667-670, 679-738, 743-820.
    Status {
        path: Option<PathBuf>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:831-837, 849-887.
    Query {
        search: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 10)]
        limit: i64,
        #[arg(short, long)]
        kind: Option<String>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:903-911, 939-1013.
    Files {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        pattern: Option<String>,
        #[arg(long, value_enum, default_value_t = FilesFormat::Tree)]
        format: FilesFormat,
        #[arg(long)]
        max_depth: Option<usize>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1110-1115, 1124-1156.
    Serve {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long)]
        mcp: bool,
        #[arg(long = "no-watch")]
        no_watch: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1167-1169, 1173-1186.
    Unlock {
        path: Option<PathBuf>,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1201-1205, 1219-1267.
    Callers {
        symbol: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1280-1284, 1298-1345.
    Callees {
        symbol: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1358-1362, 1374-1439.
    Impact {
        symbol: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    // Upstream flags/output shape: upstream bin/codegraph.ts:1462-1469, 1479-1582.
    Affected {
        files: Vec<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 5)]
        depth: usize,
        #[arg(short, long)]
        filter: Option<String>,
    },
    // New analysis surface (not in the v1.0.1 pin): forward file-dependency
    // cycle detection. Ports `findCircularDependencies`
    // (upstream graph/queries.ts:225-263).
    Check {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short = 'j', long = "json")]
        json: bool,
    },
    Export {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short = 'o', long = "out")]
        out: Option<PathBuf>,
        #[arg(long = "no-centrality")]
        no_centrality: bool,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1864-1870, 1871-1920.
    // `--global`/`--local` are convenience aliases for `--location` (task spec).
    Install {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
        #[arg(long = "no-permissions")]
        no_permissions: bool,
        #[arg(long = "print-config")]
        print_config: Option<String>,
    },
    // Upstream flags/output: upstream bin/codegraph.ts:1931-1935, 1936-1956.
    Uninstall {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Print the codegraph version.
    Version,
    /// Generate shell completion scripts (bash, zsh, fish, powershell, elvish).
    Completions {
        shell: Shell,
    },
    /// Update codegraph in place to the latest GitHub release.
    SelfUpdate {
        /// Check for a newer release without installing it.
        #[arg(long)]
        check: bool,
        /// Reinstall even if already on the latest version.
        #[arg(long)]
        force: bool,
        /// Update to a specific version tag (e.g. v0.2.0) instead of latest.
        #[arg(long)]
        tag: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FilesFormat {
    Tree,
    Flat,
    Grouped,
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init { path } => cmd_init(path),
        Command::Uninit { path, force } => cmd_uninit(path, force),
        Command::Index {
            path,
            force,
            quiet,
            verbose,
        } => cmd_index(path, force, quiet, verbose),
        Command::Sync { path, quiet } => cmd_sync(path, quiet),
        Command::Status { path, json } => cmd_status(path, json),
        Command::Query {
            search,
            path,
            limit,
            kind,
            json,
        } => cmd_query(search, path, limit, kind, json),
        Command::Files {
            path,
            filter,
            pattern,
            format,
            max_depth,
            json,
        } => cmd_files(path, filter, pattern, format, max_depth, json),
        Command::Serve {
            path,
            mcp,
            no_watch,
        } => cmd_serve(path, mcp, no_watch),
        Command::Unlock { path } => cmd_unlock(path),
        Command::Callers {
            symbol,
            path,
            limit,
            json,
        } => cmd_callers(symbol, path, limit, json),
        Command::Callees {
            symbol,
            path,
            limit,
            json,
        } => cmd_callees(symbol, path, limit, json),
        Command::Impact {
            symbol,
            path,
            depth,
            json,
        } => cmd_impact(symbol, path, depth, json),
        Command::Affected {
            files,
            path,
            depth,
            filter,
        } => cmd_affected(files, path, depth, filter),
        Command::Check { path, json } => cmd_check(path, json),
        Command::Export {
            path,
            out,
            no_centrality,
        } => cmd_export(path, out, no_centrality),
        Command::Install {
            target,
            location,
            global,
            local,
            yes,
            no_permissions,
            print_config,
        } => installer::run_install(installer::InstallArgs {
            target,
            location: location_flag(location, global, local),
            yes,
            permissions: if no_permissions { Some(false) } else { None },
            print_config,
        }),
        Command::Uninstall {
            target,
            location,
            global,
            local,
            yes,
        } => installer::run_uninstall(installer::UninstallArgs {
            target,
            location: location_flag(location, global, local),
            yes,
        }),
        Command::Version => {
            println!("codegraph {VERSION}");
            Ok(())
        }
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "codegraph", &mut io::stdout());
            Ok(())
        }
        Command::SelfUpdate { check, force, tag } => cmd_self_update(check, force, tag),
    }
}

/// Fold the `--global`/`--local` convenience flags into a `--location` string.
fn location_flag(location: Option<String>, global: bool, local: bool) -> Option<String> {
    if let Some(loc) = location {
        return Some(loc);
    }
    if global {
        return Some("global".to_string());
    }
    if local {
        return Some("local".to_string());
    }
    None
}

fn cmd_self_update(check: bool, force: bool, tag: Option<String>) -> Result<()> {
    use self_update::cargo_crate_version;

    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner("sunerpy")
        .repo_name("codegraph-rust")
        .bin_name("codegraph")
        .current_version(cargo_crate_version!())
        .show_download_progress(true)
        .no_confirm(force);
    if let Some(tag) = &tag {
        builder.target_version_tag(tag);
    }
    let updater = builder
        .build()
        .context("configuring the self-update backend")?;

    if check {
        let latest = updater
            .get_latest_release()
            .context("querying the latest GitHub release")?;
        let current = cargo_crate_version!();
        if self_update::version::bump_is_greater(current, &latest.version).unwrap_or(false) {
            println!("codegraph {current} -> {} available", latest.version);
            println!("run `codegraph self-update` to install it");
        } else {
            println!("codegraph {current} is up to date");
        }
        return Ok(());
    }

    let status = updater.update().context("performing the self-update")?;
    if status.updated() {
        println!("Updated codegraph to {}", status.version());
    } else {
        println!("codegraph {} is already up to date", status.version());
    }
    Ok(())
}

fn cmd_init(path: Option<PathBuf>) -> Result<()> {
    let project = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    if is_initialized(&project) {
        println!("Already initialized in {}", project.display());
        println!("Use \"codegraph index\" to re-index or \"codegraph sync\" to update");
        return Ok(());
    }
    fs::create_dir_all(codegraph_dir(&project))
        .with_context(|| format!("creating {}", codegraph_dir(&project).display()))?;
    let result = index_project(&project, true, false)?;
    println!("Initialized in {}", project.display());
    print_index_result(&result);
    Ok(())
}

fn cmd_uninit(path: Option<PathBuf>, force: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    if !force {
        bail!("refusing to delete .codegraph without --force");
    }
    fs::remove_dir_all(codegraph_dir(&project))
        .with_context(|| format!("removing {}", codegraph_dir(&project).display()))?;
    println!("Removed CodeGraph from {}", project.display());
    Ok(())
}

fn cmd_index(path: Option<PathBuf>, force: bool, quiet: bool, verbose: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    if force {
        remove_db_files(&project)?;
    }
    let result = index_project(&project, true, verbose)?;
    if !quiet {
        print_index_result(&result);
    }
    if result.files_errored > 0 {
        bail!("index completed with {} file errors", result.files_errored);
    }
    Ok(())
}

fn cmd_sync(path: Option<PathBuf>, quiet: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    // True single-file incremental sync (P0, docs/optimization-analysis.md §1).
    // sync_project_once self-discovers candidate files via scan_project, so it works
    // for a cold CLI invocation with no daemon. Hash-gated skip + per-file delete/reinsert
    // + full re-resolve makes the result equivalent to `index --force`.
    let outcome = codegraph_watch::sync_project_once(&project)?;
    if !quiet {
        println!(
            "Synced: {} reindexed, {} skipped (unchanged), {} removed in {}",
            format_number(outcome.files_reindexed as i64),
            format_number(outcome.files_skipped_unchanged as i64),
            format_number(outcome.files_removed as i64),
            format_duration(outcome.duration_ms as i64)
        );
    }
    Ok(())
}

fn cmd_status(path: Option<PathBuf>, json_output: bool) -> Result<()> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    let project = resolve_project_path_optional(&start);
    if !is_initialized(&project) {
        if json_output {
            print_json(&json!({
                "initialized": false,
                "version": VERSION,
                "projectPath": project,
                "indexPath": codegraph_dir(&project),
                "lastIndexed": null,
            }))?;
        } else {
            println!("\nCodeGraph Status\n");
            println!("Project: {}", project.display());
            println!("Not initialized");
            println!("Run \"codegraph init\" to initialize");
        }
        return Ok(());
    }

    let store = open_store(&project)?;
    let counts = store.counts()?;
    let nodes_by_kind = store.node_counts_by_kind()?;
    let files_by_language = store.file_counts_by_language()?;
    let db_size = fs::metadata(db_path(&project))
        .map(|m| m.len())
        .unwrap_or(0);
    let last_indexed = latest_indexed_at(&store)?;
    let built_with_version = store.get_project_metadata("indexed_with_version")?;
    let built_with_extraction_version = store
        .get_project_metadata("indexed_with_extraction_version")?
        .and_then(|v| v.parse::<i64>().ok());
    let reindex_recommended = last_indexed.is_some()
        && built_with_extraction_version.map_or(true, |v| v < EXTRACTION_VERSION);

    if json_output {
        print_json(&json!({
            "initialized": true,
            "version": VERSION,
            "projectPath": project,
            "indexPath": codegraph_dir(&project),
            "lastIndexed": last_indexed.map(iso_like_millis),
            "fileCount": counts.file_count,
            "nodeCount": counts.node_count,
            "edgeCount": counts.edge_count,
            "dbSizeBytes": db_size,
            "backend": "rusqlite",
            "journalMode": journal_mode(&store)?,
            "nodesByKind": map_counts(nodes_by_kind.clone()),
            "languages": files_by_language.iter().filter(|(_, c)| *c > 0).map(|(l, _)| l).collect::<Vec<_>>(),
            "pendingChanges": { "added": 0, "modified": 0, "removed": 0 },
            "worktreeMismatch": null,
            "index": {
                "builtWithVersion": built_with_version,
                "builtWithExtractionVersion": built_with_extraction_version,
                "currentExtractionVersion": EXTRACTION_VERSION,
                "reindexRecommended": reindex_recommended,
            },
        }))?;
        return Ok(());
    }

    println!("\nCodeGraph Status\n");
    println!("Project: {}\n", project.display());
    println!("Index Statistics:");
    println!("  Files:     {}", format_number(counts.file_count));
    println!("  Nodes:     {}", format_number(counts.node_count));
    println!("  Edges:     {}", format_number(counts.edge_count));
    println!("  DB Size:   {:.2} MB", db_size as f64 / 1024.0 / 1024.0);
    println!("  Backend:   rusqlite - bundled SQLite");
    println!("  Journal:   {}\n", journal_mode(&store)?);
    println!("Nodes by Kind:");
    for (kind, count) in nodes_by_kind {
        println!("  {kind:15} {}", format_number(count));
    }
    println!("\nFiles by Language:");
    for (language, count) in files_by_language {
        println!("  {language:15} {}", format_number(count));
    }
    println!("\nIndex is up to date\n");
    Ok(())
}

fn cmd_query(
    search: String,
    path: Option<PathBuf>,
    limit: i64,
    kind: Option<String>,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let kinds = kind
        .as_deref()
        .map(parse_node_kind)
        .transpose()?
        .into_iter()
        .collect();
    let results = search_nodes(
        &store,
        &search,
        &SearchOptions {
            kinds,
            languages: Vec::new(),
            limit: Some(limit),
            offset: Some(0),
        },
        &project_name_tokens(&project),
    )?;
    if json_output {
        let output = results.iter().map(SearchOutput::from).collect::<Vec<_>>();
        print_json_pretty(&output)?;
        return Ok(());
    }
    if results.is_empty() {
        println!("No results found for \"{search}\"");
    } else {
        println!("\nSearch Results for \"{search}\":\n");
        for result in results {
            println!(
                "{:<12}{} ({:.0}%)",
                result.node.kind,
                result.node.name,
                result.score * 100.0
            );
            println!("  {}:{}", result.node.file_path, result.node.start_line);
            if let Some(signature) = &result.node.signature {
                println!("  {signature}");
            }
            println!();
        }
    }
    Ok(())
}

fn cmd_files(
    path: Option<PathBuf>,
    filter: Option<String>,
    pattern: Option<String>,
    format: FilesFormat,
    max_depth: Option<usize>,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let mut files = store.all_files()?;
    if let Some(filter) = filter {
        let alt = format!("./{filter}");
        files.retain(|f| f.path.starts_with(&filter) || f.path.starts_with(&alt));
    }
    if let Some(pattern) = pattern {
        files.retain(|f| glob_matches(&pattern, &f.path));
    }
    if json_output {
        let output = files.iter().map(FileOutput::from).collect::<Vec<_>>();
        print_json_pretty(&output)?;
        return Ok(());
    }
    if files.is_empty() {
        println!("No files found matching the criteria.");
        return Ok(());
    }
    match format {
        FilesFormat::Flat => print_files_flat(&files),
        FilesFormat::Grouped => print_files_grouped(&files),
        FilesFormat::Tree => print_files_tree(&files, max_depth),
    }
    Ok(())
}

fn cmd_serve(path: Option<PathBuf>, mcp: bool, no_watch: bool) -> Result<()> {
    let project = path.map(|p| resolve_project_path_optional(&absolute_path(p)));
    if no_watch {
        std::env::set_var("CODEGRAPH_NO_WATCH", "1");
    }
    if mcp {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server = McpServer::new(project);
        return server
            .run(BufReader::new(stdin.lock()), stdout.lock())
            .context("running MCP stdio server");
    }
    eprintln!("\nCodeGraph daemon/watch server\n");
    eprintln!("Daemon and watcher startup is wired here for tasks 24/25.");
    eprintln!("Use `codegraph serve --mcp` to start the committed MCP stdio server.");
    Ok(())
}

fn cmd_unlock(path: Option<PathBuf>) -> Result<()> {
    let project = resolve_required_project(path)?;
    let daemon_lock = codegraph_daemon::daemon_pid_path(&project);
    let daemon_removed = daemon_lock.exists() && codegraph_daemon::unlock_project(&project);
    let lock = codegraph_dir(&project).join("codegraph.lock");
    if !lock.exists() && !daemon_removed {
        println!("No lock file found - nothing to do");
        return Ok(());
    }
    if lock.exists() {
        fs::remove_file(&lock).with_context(|| format!("removing {}", lock.display()))?;
    }
    println!("Removed lock file. You can now run indexing again.");
    Ok(())
}

fn cmd_callers(
    symbol: String,
    path: Option<PathBuf>,
    limit: usize,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let nodes = related_nodes_for_symbol(&store, &project, &symbol, limit, Related::Callers)?;
    if json_output {
        print_json_pretty(&json!({ "symbol": symbol, "callers": nodes }))?;
    } else {
        print_related("Callers", &symbol, &nodes);
    }
    Ok(())
}

fn cmd_callees(
    symbol: String,
    path: Option<PathBuf>,
    limit: usize,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let nodes = related_nodes_for_symbol(&store, &project, &symbol, limit, Related::Callees)?;
    if json_output {
        print_json_pretty(&json!({ "symbol": symbol, "callees": nodes }))?;
    } else {
        print_related("Callees", &symbol, &nodes);
    }
    Ok(())
}

fn cmd_impact(
    symbol: String,
    path: Option<PathBuf>,
    depth: usize,
    json_output: bool,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let depth = depth.clamp(1, 10);
    let matches = symbol_matches(&store, &project, &symbol)?;
    if matches.is_empty() {
        println!("Symbol \"{symbol}\" not found");
        return Ok(());
    }
    let traverser = GraphTraverser::new(&store);
    let mut nodes = HashMap::new();
    let mut edge_keys = HashSet::new();
    for node in exact_or_top_matches(&matches, &symbol) {
        let impact = traverser.get_impact_radius(&node.id, depth)?;
        for (id, node) in impact.nodes {
            nodes.insert(id, node);
        }
        for edge in impact.edges {
            edge_keys.insert((edge.source, edge.target, edge.kind));
        }
    }
    let affected = nodes.values().map(NodeSummary::from).collect::<Vec<_>>();
    if json_output {
        print_json_pretty(&json!({
            "symbol": symbol,
            "depth": depth,
            "nodeCount": affected.len(),
            "edgeCount": edge_keys.len(),
            "affected": affected,
        }))?;
    } else {
        println!(
            "\nImpact of changing \"{}\" - {} affected symbols:\n",
            symbol,
            affected.len()
        );
        print_by_file(&affected);
    }
    Ok(())
}

fn cmd_affected(
    files: Vec<String>,
    path: Option<PathBuf>,
    depth: usize,
    filter: Option<String>,
) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    if files.is_empty() {
        println!("No files provided. Use file arguments.");
        return Ok(());
    }
    let mut affected = HashSet::new();
    let mut traversed = HashSet::new();
    for file in &files {
        if is_test_file(file, filter.as_deref()) {
            affected.insert(file.clone());
            continue;
        }
        let mut queue = VecDeque::from([(file.clone(), 0usize)]);
        let mut visited = HashSet::from([file.clone()]);
        while let Some((current, current_depth)) = queue.pop_front() {
            if current_depth >= depth {
                continue;
            }
            for dependent in store.dependent_file_paths(&current)? {
                if !visited.insert(dependent.clone()) {
                    continue;
                }
                traversed.insert(dependent.clone());
                if is_test_file(&dependent, filter.as_deref()) {
                    affected.insert(dependent);
                } else {
                    queue.push_back((dependent, current_depth + 1));
                }
            }
        }
    }
    let mut sorted = affected.into_iter().collect::<Vec<_>>();
    sorted.sort();
    print_json_pretty(&json!({
        "changedFiles": files,
        "affectedTests": sorted,
        "totalDependentsTraversed": traversed.len(),
    }))?;
    Ok(())
}

fn cmd_check(path: Option<PathBuf>, json_output: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let traverser = GraphTraverser::new(&store);
    let cycles = traverser.find_circular_dependencies()?;
    if json_output {
        print_json_pretty(&json!({ "cycles": cycles }))?;
    } else if cycles.is_empty() {
        println!("No circular dependencies found");
    } else {
        println!("\nFound {} circular dependencies:\n", cycles.len());
        for cycle in &cycles {
            let mut chain = cycle.clone();
            if let Some(first) = cycle.first() {
                chain.push(first.clone());
            }
            println!("  {}", chain.join(" \u{2192} "));
        }
    }
    Ok(())
}

fn cmd_export(path: Option<PathBuf>, out: Option<PathBuf>, no_centrality: bool) -> Result<()> {
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let graph = codegraph_graph::export::node_link_graph_opts(&store, !no_centrality)?;
    let rendered = serde_json::to_string_pretty(&graph)?;
    match out {
        Some(out_path) => {
            fs::write(&out_path, rendered.as_bytes())
                .with_context(|| format!("writing graph export to {}", out_path.display()))?;
            let counts = store.counts()?;
            eprintln!(
                "Exported {} nodes / {} edges to {}",
                counts.node_count,
                counts.edge_count,
                out_path.display()
            );
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

#[derive(Debug)]
struct IndexSummary {
    files_indexed: i64,
    files_skipped: i64,
    files_errored: i64,
    nodes_created: i64,
    edges_created: i64,
    duration_ms: i64,
}

fn index_project(project: &Path, clear_first: bool, verbose: bool) -> Result<IndexSummary> {
    let started = std::time::Instant::now();
    if clear_first {
        remove_db_files(project)?;
    }
    fs::create_dir_all(codegraph_dir(project))?;
    let config = codegraph_core::config::get_config();
    let options = ExtractOptions {
        max_file_size: config.indexing.max_file_size,
        ignore_dirs: config.indexing.ignore_dirs.clone(),
        parallel: true,
    };
    let files = codegraph_extract::engine::scan_project(project, &options)?;
    let mut store = open_store(project)?;
    let before = store.counts()?;
    let mut files_indexed = 0;
    let mut files_skipped = 0;
    let mut files_errored = 0;

    // Stream the graph to the store in capped batches instead of holding the whole
    // project in memory. Equivalence with the all-at-once path is byte-for-byte and
    // load-bearing, so the original insert order is reproduced exactly:
    //   1. nodes flush in sorted `scan_project` file order, each file's nodes in
    //      emission order — the resolver's name-matcher tie-break reads candidates
    //      back in node rowid order (`codegraph-resolve` `order_candidates`);
    //   2. ALL nodes are written before ANY edge, because `insert_edges` drops edges
    //      whose endpoints are absent;
    //   3. edges then refs replay in the same file order, so their autoincrement
    //      rowids match the all-at-once path.
    // Edges/refs cannot flush during the node pass (rule 2) and would dominate memory,
    // so they spill to a temp file and stream back in a second batched pass.
    const NODE_FLUSH_ROWS: usize = 10_000;
    const EDGE_FLUSH_ROWS: usize = 20_000;
    const REF_FLUSH_ROWS: usize = 20_000;
    const RESOLVE_BATCH_ROWS: usize = 5_000;

    let mut spill = SpillWriter::new(codegraph_dir(project))?;
    let mut pending_nodes: Vec<Node> = Vec::with_capacity(NODE_FLUSH_ROWS);

    for relative in files {
        if verbose {
            println!("indexing {relative}");
        }
        let full = project.join(&relative);
        let source = fs::read_to_string(&full)
            .with_context(|| format!("reading source file {}", full.display()))?;
        let metadata = fs::metadata(&full)
            .with_context(|| format!("reading metadata for {}", full.display()))?;
        let mut result = extract_file(project, &relative)?;
        let errors = result.errors.clone();
        if errors.is_empty() {
            files_indexed += 1;
        } else if result.nodes.is_empty() {
            files_skipped += 1;
        } else {
            files_errored += 1;
        }
        let file = FileRecord {
            path: relative.clone(),
            content_hash: hash_content(&source),
            language: detect_language(&relative),
            size: metadata.len() as i64,
            modified_at: modified_millis(&metadata),
            indexed_at: now_millis(),
            node_count: result
                .nodes
                .iter()
                .filter(|n| n.file_path == relative)
                .count() as i64,
            errors,
        };

        store.upsert_file(&file)?;

        pending_nodes.append(&mut result.nodes);
        if pending_nodes.len() >= NODE_FLUSH_ROWS {
            store.upsert_nodes(&pending_nodes)?;
            pending_nodes.clear();
        }

        spill.write_edges(&result.edges)?;
        spill.write_refs(&result.unresolved_references)?;
    }

    if !pending_nodes.is_empty() {
        store.upsert_nodes(&pending_nodes)?;
    }
    drop(pending_nodes);

    let mut spill = spill.into_reader()?;
    spill.replay_edges(EDGE_FLUSH_ROWS, |batch| {
        store.insert_edges(batch).map_err(anyhow::Error::from)
    })?;
    spill.replay_refs(REF_FLUSH_ROWS, |batch| {
        store
            .insert_unresolved_refs(batch)
            .map_err(anyhow::Error::from)
    })?;
    spill.cleanup();

    let mut resolver = ReferenceResolver::new(project.to_string_lossy());
    // Detect frameworks then run their per-file extract (route/component/handler
    // nodes + refs) BEFORE resolution, mirroring the upstream tree-sitter.ts:4796-4819
    // framework-extraction pass feeding the resolution pipeline.
    {
        let context =
            codegraph_resolve::StoreResolutionContext::new(&store, project.to_string_lossy());
        resolver.initialize(&context);
    }
    if resolver.has_framework_resolvers() {
        let relative_files = store
            .all_files()?
            .into_iter()
            .map(|f| f.path)
            .collect::<Vec<_>>();
        resolver.extract_and_persist_frameworks(&mut store, &relative_files)?;
    }
    resolver.resolve_and_persist_batched(&mut store, RESOLVE_BATCH_ROWS)?;
    // Cross-file framework finalization (NestJS RouterModule prefixing) after
    // resolution, mirroring the upstream index.ts:358 runPostExtract.
    resolver.run_post_extract(&mut store)?;
    store.set_project_metadata("indexed_with_version", VERSION)?;
    store.set_project_metadata(
        "indexed_with_extraction_version",
        &EXTRACTION_VERSION.to_string(),
    )?;
    store.compact()?;
    let after = store.counts()?;
    Ok(IndexSummary {
        files_indexed,
        files_skipped,
        files_errored,
        nodes_created: after.node_count - before.node_count,
        edges_created: after.edge_count - before.edge_count,
        duration_ms: started.elapsed().as_millis() as i64,
    })
}

/// On-disk spill for extracted edges and unresolved refs during a full index.
///
/// They cannot be persisted during the node pass (all nodes must precede any edge)
/// and would dominate memory, so they are written as newline-delimited JSON in
/// extraction order and streamed back in capped batches, preserving the exact
/// insert order the all-at-once path produced.
struct SpillWriter {
    edges_path: PathBuf,
    refs_path: PathBuf,
    edges: io::BufWriter<fs::File>,
    refs: io::BufWriter<fs::File>,
}

impl SpillWriter {
    fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let edges_path = dir.join(".index-edges.spill");
        let refs_path = dir.join(".index-refs.spill");
        let edges = io::BufWriter::new(
            fs::File::create(&edges_path)
                .with_context(|| format!("creating spill file {}", edges_path.display()))?,
        );
        let refs = io::BufWriter::new(
            fs::File::create(&refs_path)
                .with_context(|| format!("creating spill file {}", refs_path.display()))?,
        );
        Ok(Self {
            edges_path,
            refs_path,
            edges,
            refs,
        })
    }

    fn write_edges(&mut self, edges: &[codegraph_core::types::Edge]) -> Result<()> {
        for edge in edges {
            serde_json::to_writer(&mut self.edges, edge)?;
            self.edges.write_all(b"\n")?;
        }
        Ok(())
    }

    fn write_refs(&mut self, refs: &[codegraph_core::types::UnresolvedRef]) -> Result<()> {
        for reference in refs {
            serde_json::to_writer(&mut self.refs, reference)?;
            self.refs.write_all(b"\n")?;
        }
        Ok(())
    }

    fn into_reader(mut self) -> Result<SpillReader> {
        self.edges.flush()?;
        self.refs.flush()?;
        Ok(SpillReader {
            edges_path: self.edges_path,
            refs_path: self.refs_path,
        })
    }
}

struct SpillReader {
    edges_path: PathBuf,
    refs_path: PathBuf,
}

impl SpillReader {
    fn replay_edges<F>(&mut self, batch_rows: usize, mut flush: F) -> Result<()>
    where
        F: FnMut(&[codegraph_core::types::Edge]) -> Result<()>,
    {
        let reader = BufReader::new(fs::File::open(&self.edges_path)?);
        let mut batch: Vec<codegraph_core::types::Edge> = Vec::with_capacity(batch_rows);
        for line in reader.lines() {
            batch.push(serde_json::from_str(&line?)?);
            if batch.len() >= batch_rows {
                flush(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            flush(&batch)?;
        }
        Ok(())
    }

    fn replay_refs<F>(&mut self, batch_rows: usize, mut flush: F) -> Result<()>
    where
        F: FnMut(&[codegraph_core::types::UnresolvedRef]) -> Result<()>,
    {
        let reader = BufReader::new(fs::File::open(&self.refs_path)?);
        let mut batch: Vec<codegraph_core::types::UnresolvedRef> = Vec::with_capacity(batch_rows);
        for line in reader.lines() {
            batch.push(serde_json::from_str(&line?)?);
            if batch.len() >= batch_rows {
                flush(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            flush(&batch)?;
        }
        Ok(())
    }

    fn cleanup(self) {
        let _ = fs::remove_file(&self.edges_path);
        let _ = fs::remove_file(&self.refs_path);
    }
}

fn print_index_result(result: &IndexSummary) {
    if result.files_indexed > 0 {
        println!("Indexed {} files", format_number(result.files_indexed));
        println!(
            "{} nodes, {} edges in {}",
            format_number(result.nodes_created),
            format_number(result.edges_created),
            format_duration(result.duration_ms)
        );
    } else if result.files_errored > 0 {
        println!(
            "Indexing failed - all {} files had errors",
            result.files_errored
        );
    } else {
        println!("No files found to index");
    }
    if result.files_skipped > 0 {
        println!("Skipped {} files", format_number(result.files_skipped));
    }
}

fn related_nodes_for_symbol(
    store: &Store,
    project: &Path,
    symbol: &str,
    limit: usize,
    related: Related,
) -> Result<Vec<NodeSummary>> {
    let matches = symbol_matches(store, project, symbol)?;
    if matches.is_empty() {
        return Ok(Vec::new());
    }
    let traverser = GraphTraverser::new(store);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in exact_or_top_matches(&matches, symbol) {
        let edges = match related {
            Related::Callers => traverser.get_callers(&node.id, 1)?,
            Related::Callees => traverser.get_callees(&node.id, 1)?,
        };
        for entry in edges {
            if seen.insert(entry.node.id.clone()) {
                out.push(NodeSummary::from(&entry.node));
            }
        }
    }
    out.truncate(limit);
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
enum Related {
    Callers,
    Callees,
}

fn symbol_matches(store: &Store, project: &Path, symbol: &str) -> Result<Vec<Node>> {
    let results = search_nodes(
        store,
        symbol,
        &SearchOptions {
            limit: Some(50),
            ..Default::default()
        },
        &project_name_tokens(project),
    )?;
    Ok(results.into_iter().map(|r| r.node).collect())
}

fn exact_or_top_matches<'a>(matches: &'a [Node], symbol: &str) -> Vec<&'a Node> {
    let exact = matches
        .iter()
        .filter(|node| {
            node.name == symbol
                || node.name.ends_with(&format!(".{symbol}"))
                || node.name.ends_with(&format!("::{symbol}"))
        })
        .collect::<Vec<_>>();
    if exact.is_empty() {
        matches.first().into_iter().collect()
    } else {
        exact
    }
}

fn open_store(project: &Path) -> Result<Store> {
    Store::open(&db_path(project)).map_err(Into::into)
}

fn is_initialized(project: &Path) -> bool {
    db_path(project).exists()
}

fn resolve_required_project(path: Option<PathBuf>) -> Result<PathBuf> {
    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    let project = resolve_project_path_optional(&start);
    if !is_initialized(&project) {
        bail!("CodeGraph not initialized in {}", project.display());
    }
    Ok(project)
}

fn resolve_project_path_optional(start: &Path) -> PathBuf {
    if is_initialized(start) {
        return start.to_path_buf();
    }
    let mut current = start;
    while let Some(parent) = current.parent() {
        if parent == current {
            break;
        }
        if is_initialized(parent) {
            return parent.to_path_buf();
        }
        current = parent;
    }
    start.to_path_buf()
}

fn absolute_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn codegraph_dir(project: &Path) -> PathBuf {
    project.join(std::env::var("CODEGRAPH_DIR").unwrap_or_else(|_| ".codegraph".to_string()))
}

fn db_path(project: &Path) -> PathBuf {
    codegraph_dir(project).join("codegraph.db")
}

fn remove_db_files(project: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{}", db_path(project).display(), suffix));
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

fn parse_node_kind(raw: &str) -> Result<NodeKind> {
    NodeKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == raw)
        .ok_or_else(|| anyhow!("unknown node kind: {raw}"))
}

fn project_name_tokens(project: &Path) -> HashSet<String> {
    project
        .file_name()
        .and_then(|n| n.to_str())
        .into_iter()
        .flat_map(|name| name.split(['-', '_', '.', ' ']))
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

fn latest_indexed_at(store: &Store) -> Result<Option<i64>> {
    Ok(store.all_files()?.iter().map(|f| f.indexed_at).max())
}

fn journal_mode(store: &Store) -> Result<String> {
    store
        .connection()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(Into::into)
}

fn map_counts(entries: Vec<(String, i64)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert(key, json!(value));
    }
    serde_json::Value::Object(map)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn modified_millis(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(now_millis)
}

fn iso_like_millis(ms: i64) -> String {
    match OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000) {
        Ok(dt) => dt.format(&Rfc3339).unwrap_or_else(|_| format!("{ms}")),
        Err(_) => format!("{ms}"),
    }
}

fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:.0}s", ms / 60_000, (ms % 60_000) as f64 / 1000.0)
    }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), value.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], value: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    match pattern[0] {
        b'*' => {
            glob_match_bytes(&pattern[1..], value)
                || (!value.is_empty() && glob_match_bytes(pattern, &value[1..]))
        }
        b'?' => {
            !value.is_empty() && value[0] != b'/' && glob_match_bytes(&pattern[1..], &value[1..])
        }
        ch => !value.is_empty() && ch == value[0] && glob_match_bytes(&pattern[1..], &value[1..]),
    }
}

fn is_test_file(file: &str, filter: Option<&str>) -> bool {
    if let Some(filter) = filter {
        return glob_matches(filter, file);
    }
    file.contains(".spec.")
        || file.contains(".test.")
        || file.contains("/__tests__/")
        || file.contains("/test/")
        || file.contains("/tests/")
        || file.contains("/e2e/")
        || file.contains("/spec/")
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn print_json_pretty<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchOutput<'a> {
    node: &'a Node,
    score: f64,
}

impl<'a> From<&'a SearchResult> for SearchOutput<'a> {
    fn from(result: &'a SearchResult) -> Self {
        Self {
            node: &result.node,
            score: result.score,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileOutput<'a> {
    path: &'a str,
    language: Language,
    node_count: i64,
    size: i64,
}

impl<'a> From<&'a FileRecord> for FileOutput<'a> {
    fn from(file: &'a FileRecord) -> Self {
        Self {
            path: &file.path,
            language: file.language,
            node_count: file.node_count,
            size: file.size,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSummary {
    name: String,
    kind: NodeKind,
    file_path: String,
    start_line: i64,
}

impl From<&Node> for NodeSummary {
    fn from(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind,
            file_path: node.file_path.clone(),
            start_line: node.start_line,
        }
    }
}

fn print_related(label: &str, symbol: &str, nodes: &[NodeSummary]) {
    if nodes.is_empty() {
        println!("No {} found for \"{}\"", label.to_lowercase(), symbol);
        return;
    }
    println!("\n{label} of \"{symbol}\" ({}):\n", nodes.len());
    for node in nodes {
        println!("{:<12}{}", node.kind, node.name);
        println!("  {}:{}\n", node.file_path, node.start_line);
    }
}

fn print_by_file(nodes: &[NodeSummary]) {
    let mut by_file: HashMap<&str, Vec<&NodeSummary>> = HashMap::new();
    for node in nodes {
        by_file.entry(&node.file_path).or_default().push(node);
    }
    let mut files = by_file.keys().copied().collect::<Vec<_>>();
    files.sort_unstable();
    for file in files {
        println!("{file}");
        for node in &by_file[file] {
            println!("  {:<12}{}:{}", node.kind, node.name, node.start_line);
        }
        println!();
    }
}

fn print_files_flat(files: &[FileRecord]) {
    println!("\nFiles ({}):\n", files.len());
    for file in files {
        println!(
            "  {} ({}, {} symbols)",
            file.path, file.language, file.node_count
        );
    }
}

fn print_files_grouped(files: &[FileRecord]) {
    println!("\nFiles by Language ({} total):\n", files.len());
    let mut by_lang: HashMap<Language, Vec<&FileRecord>> = HashMap::new();
    for file in files {
        by_lang.entry(file.language).or_default().push(file);
    }
    let mut groups = by_lang.into_iter().collect::<Vec<_>>();
    groups.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    for (language, mut group) in groups {
        group.sort_by(|a, b| a.path.cmp(&b.path));
        println!("{} ({}):", language, group.len());
        for file in group {
            println!("  {} ({} symbols)", file.path, file.node_count);
        }
        println!();
    }
}

fn print_files_tree(files: &[FileRecord], max_depth: Option<usize>) {
    println!("\nProject Structure ({} files):\n", files.len());
    for file in files {
        let depth = file.path.matches('/').count() + 1;
        if max_depth.map_or(true, |max| depth <= max) {
            println!(
                "  {} ({}, {} symbols)",
                file.path, file.language, file.node_count
            );
        }
    }
}
