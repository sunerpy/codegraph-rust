//! Single `codegraph` CLI binary.
//!
//! This crate owns process bootstrap: load config fail-fast, initialize tracing,
//! keep the `WorkerGuard` alive, then run the requested command. Library crates
//! only emit tracing events.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use codegraph_core::config::init_config;
use codegraph_core::logger::{init_logger, LoggerConfig};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{ExtractionResult, FileRecord, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_source, ExtractOptions};
use codegraph_graph::graph::{GodotReach, GraphTraverser};
use codegraph_graph::query::{search_nodes, SearchOptions};
use codegraph_mcp::{McpServer, RunUntilAdoption};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::queries::SearchResult;
use codegraph_store::Store;
use indicatif::{ParallelProgressIterator, ProgressBar, ProgressDrawTarget, ProgressStyle};
use rayon::prelude::*;
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
            Command::Init { path, .. }
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
            | Command::Check { path, .. }
            | Command::Audit { path, .. } => path.clone(),
            Command::Export { path, .. } => path.clone(),
            Command::PromptHook { path, .. } => path.clone(),
            // install/uninstall/skill are not project-scoped — bootstrap from cwd.
            Command::Install { .. }
            | Command::Uninstall { .. }
            | Command::Skill { .. }
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
        /// Also write project-level MCP config for these agents (csv ids,
        /// `auto`, `all`, `none`). Defaults to `none` (index only). Editors that
        /// launch the server from a non-project CWD (Kiro, Cursor) need this to
        /// get the project's absolute `--path`.
        #[arg(short, long, default_value = "none")]
        target: String,
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
        /// Filter to files under this directory (path prefix).
        #[arg(long, value_name = "DIR")]
        filter: Option<String>,
        /// Filter to files of this language (matches `status` names, e.g. gdscript, godot_scene).
        #[arg(long, value_name = "LANG")]
        language: Option<String>,
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
        /// Serve MCP over streamable-HTTP (rmcp) instead of stdio. Requires an
        /// already-indexed `--path` (or an indexed cwd); binds localhost.
        #[arg(long)]
        http: bool,
        /// Address to bind the streamable-HTTP server to (localhost only).
        #[arg(long = "http-addr", default_value = "127.0.0.1:8111")]
        http_addr: String,
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
    /// Read-only Godot resource audit: orphan resources, dangling references,
    /// and reverse-dependency impact. Computed from the existing graph + disk
    /// checks; adds no extraction and is separate from `check`.
    Audit {
        /// Project root (NOT a result filter; use --include/--exclude to narrow).
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Report `.tres`/`.tscn` resources nothing references.
        #[arg(long)]
        orphans: bool,
        /// Report path references whose target is missing on disk.
        #[arg(long)]
        dangling: bool,
        /// Report what references the given changed resource/script path.
        #[arg(long, value_name = "PATH")]
        impact: Option<String>,
        /// With --impact: emit a derived load/open plan (loadScripts/loadResources/openScenes/reasons).
        #[arg(long = "verify-plan", requires = "impact")]
        verify_plan: bool,
        /// Keep only results whose path is under this prefix (repeatable).
        #[arg(long, value_name = "PREFIX")]
        include: Vec<String>,
        /// Drop results whose path is under this prefix, e.g. addons/ (repeatable).
        #[arg(long, value_name = "PREFIX")]
        exclude: Vec<String>,
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
        #[arg(long = "prompt-hook")]
        prompt_hook: bool,
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
    /// Manage the embedded CodeGraph agent skill (install/update/uninstall/status).
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Print the codegraph version.
    Version,
    /// Generate shell completion scripts (bash, zsh, fish, powershell, elvish).
    Completions {
        shell: Shell,
        /// Install the script to the shell's completion location instead of printing it.
        #[arg(long)]
        install: bool,
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
    /// Emit deterministic `codegraph_explore` output for a query (NO LLM). Query
    /// from `--query`/positional or stdin; project is the nearest `.codegraph/`.
    #[command(hide = true)]
    PromptHook {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long)]
        query: Option<String>,
        #[arg(value_name = "QUERY")]
        query_positional: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FilesFormat {
    Tree,
    Flat,
    Grouped,
}

#[derive(Debug, Subcommand)]
enum SkillAction {
    /// Install the embedded CodeGraph skill into the agent's skill directory.
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
    },
    /// Refresh an installed skill to the embedded version (use --force to overwrite local edits).
    Update {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
        #[arg(long)]
        force: bool,
    },
    /// Remove the installed CodeGraph skill.
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
    /// Report installed-skill status per agent.
    Status {
        #[arg(short, long)]
        target: Option<String>,
        #[arg(short, long)]
        location: Option<String>,
        #[arg(long, conflicts_with_all = ["local", "location"])]
        global: bool,
        #[arg(long, conflicts_with = "location")]
        local: bool,
    },
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init { path, target } => cmd_init(path, &target),
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
            language,
            pattern,
            format,
            max_depth,
            json,
        } => cmd_files(path, filter, language, pattern, format, max_depth, json),
        Command::Serve {
            path,
            mcp,
            no_watch,
            http,
            http_addr,
        } => cmd_serve(path, mcp, no_watch, http, http_addr),
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
        Command::Audit {
            path,
            orphans,
            dangling,
            impact,
            verify_plan,
            include,
            exclude,
            json,
        } => cmd_audit(AuditArgs {
            path,
            orphans,
            dangling,
            impact,
            verify_plan,
            include,
            exclude,
            json_output: json,
        }),
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
            prompt_hook,
            print_config,
        } => installer::run_install(installer::InstallArgs {
            target,
            location: location_flag(location, global, local),
            yes,
            permissions: if no_permissions { Some(false) } else { None },
            front_load_hook: prompt_hook,
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
        Command::Skill { action } => match action {
            SkillAction::Install {
                target,
                location,
                global,
                local,
                yes,
            } => installer::run_skill_install(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes,
                force: false,
            }),
            SkillAction::Update {
                target,
                location,
                global,
                local,
                force,
            } => installer::run_skill_update(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes: false,
                force,
            }),
            SkillAction::Uninstall {
                target,
                location,
                global,
                local,
                yes,
            } => installer::run_skill_uninstall(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes,
                force: false,
            }),
            SkillAction::Status {
                target,
                location,
                global,
                local,
            } => installer::run_skill_status(installer::SkillArgs {
                target,
                location: location_flag(location, global, local),
                yes: false,
                force: false,
            }),
        },
        Command::Version => {
            println!("codegraph {VERSION}");
            Ok(())
        }
        Command::Completions { shell, install } => {
            if install {
                install_completions(shell)
            } else {
                let mut cmd = Cli::command();
                generate(shell, &mut cmd, "codegraph", &mut io::stdout());
                Ok(())
            }
        }
        Command::SelfUpdate { check, force, tag } => cmd_self_update(check, force, tag),
        Command::PromptHook {
            path,
            query,
            query_positional,
        } => cmd_prompt_hook(path, query.or(query_positional)),
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

fn generate_completion_bytes(shell: Shell) -> Vec<u8> {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    generate(shell, &mut cmd, "codegraph", &mut buf);
    buf
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf> {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| anyhow!("cannot resolve home directory (HOME/USERPROFILE unset)"))
}

fn write_completion_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating completion directory {}", parent.display()))?;
    }
    fs::write(path, bytes)
        .with_context(|| format!("writing completion script {}", path.display()))?;
    Ok(())
}

fn completion_target(shell: Shell) -> Result<PathBuf> {
    Ok(match shell {
        Shell::Bash => {
            let base = env_path("XDG_DATA_HOME")
                .unwrap_or_else(|| home_dir().unwrap_or_default().join(".local/share"));
            base.join("bash-completion/completions/codegraph")
        }
        Shell::Zsh => home_dir()?.join(".zfunc/_codegraph"),
        Shell::Fish => home_dir()?.join(".config/fish/completions/codegraph.fish"),
        Shell::PowerShell => {
            let base = env_path("LOCALAPPDATA")
                .unwrap_or_else(|| home_dir().unwrap_or_default().join(".local/share"));
            base.join("codegraph/completion.ps1")
        }
        Shell::Elvish => home_dir()?.join(".config/codegraph/completion.elv"),
        _ => bail!("unsupported shell for --install"),
    })
}

fn powershell_profile_path() -> Result<PathBuf> {
    if let Some(p) = env_path("CODEGRAPH_PS_PROFILE") {
        return Ok(p);
    }
    let user = env_path("USERPROFILE").or_else(|| env_path("HOME")).ok_or_else(|| {
        anyhow!("cannot resolve PowerShell profile (set CODEGRAPH_PS_PROFILE, USERPROFILE, or HOME)")
    })?;
    Ok(user.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"))
}

fn append_dot_source_once(profile: &Path, script: &Path) -> Result<bool> {
    let line = format!(". \"{}\"", script.display());
    let existing = fs::read_to_string(profile).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    if let Some(parent) = profile.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating profile directory {}", parent.display()))?;
    }
    let mut prefix = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        prefix.push('\n');
    }
    fs::write(profile, format!("{existing}{prefix}{line}\n"))
        .with_context(|| format!("appending dot-source line to {}", profile.display()))?;
    Ok(true)
}

fn install_completions(shell: Shell) -> Result<()> {
    let target = completion_target(shell)?;
    let bytes = generate_completion_bytes(shell);
    write_completion_file(&target, &bytes)?;
    println!("Installed {shell} completions to {}", target.display());

    match shell {
        // PowerShell's `using namespace` header is legal only at file start, so write a separate file and dot-source it (never append inline to $PROFILE).
        Shell::PowerShell => {
            let profile = powershell_profile_path()?;
            let added = append_dot_source_once(&profile, &target)?;
            if added {
                println!("Added dot-source line to {}", profile.display());
            } else {
                println!(
                    "Profile already sources the completion script: {}",
                    profile.display()
                );
            }
            println!("Restart your shell (or run `. $PROFILE`) to load completions.");
            println!("Press Ctrl+Space to trigger menu completion (Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete).");
        }
        Shell::Zsh => {
            println!("Add `fpath+=~/.zfunc` before `compinit` in your ~/.zshrc if it is not already there.");
            println!("Restart your shell to load completions.");
        }
        Shell::Elvish => {
            println!(
                "Add `eval (slurp < {})` to your ~/.config/elvish/rc.elv to load completions.",
                target.display()
            );
        }
        _ => {
            println!("Restart your shell to load completions.");
        }
    }
    Ok(())
}

/// Format the GitHub release tag to target from a release's bare semver.
///
/// `self_update`'s `Release.version` (from `get_latest_release()`) is the bare
/// semver with NO leading `v` (e.g. `0.15.0`), but this repo tags releases as
/// `v{semver}` (e.g. `v0.15.0`), and `target_version_tag` must match the tag
/// exactly. This bridges the two and is idempotent on an already-`v`-prefixed
/// input so it's safe regardless of which form the backend hands us.
fn latest_update_tag(latest_version: &str) -> String {
    let bare = latest_version.strip_prefix('v').unwrap_or(latest_version);
    format!("v{bare}")
}

/// Decide whether `self-update` should skip the download/replace flow because
/// the running binary is already current.
///
/// Returns `true` (skip, print "up to date", do NOT prompt/download) only when:
/// no explicit `--tag` was given, `--force` was not passed, and `latest` is not
/// a greater semver than `current`. An explicit tag or `--force` always proceeds
/// (returns `false`), and a genuinely newer release also proceeds.
fn should_skip_update(current: &str, latest: &str, force: bool, has_explicit_tag: bool) -> bool {
    if force || has_explicit_tag {
        return false;
    }
    !self_update::version::bump_is_greater(current, latest).unwrap_or(false)
}

fn cmd_self_update(check: bool, force: bool, tag: Option<String>) -> Result<()> {
    use self_update::cargo_crate_version;

    let configure = || {
        let mut builder = self_update::backends::github::Update::configure();
        builder
            .repo_owner("sunerpy")
            .repo_name("codegraph-rust")
            .bin_name("codegraph")
            .current_version(cargo_crate_version!())
            .show_download_progress(true)
            .no_confirm(force);
        builder
    };

    // `--check`: just report whether a newer release exists, never install.
    if check {
        let updater = configure()
            .build()
            .context("configuring the self-update backend")?;
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

    // Resolve the tag to install. With an explicit `--tag` we honor it verbatim.
    //
    // Without `--tag` we must resolve the LATEST release ourselves and pin it via
    // `target_version_tag`. Otherwise `self_update`'s no-target path filters
    // releases by semver-compatibility and installs the *first compatible* one,
    // which on a 0.x line advances a single minor per run (e.g. 0.5.2 -> 0.5.3,
    // then 0.5.3 -> 0.14.0) instead of jumping straight to newest. Pinning the
    // latest tag bypasses that stepping so one run lands on the newest release.
    let target_tag = match tag {
        Some(t) => t,
        None => {
            let probe = configure()
                .build()
                .context("configuring the self-update backend")?;
            let latest = probe
                .get_latest_release()
                .context("querying the latest GitHub release")?;
            let current = cargo_crate_version!();
            if should_skip_update(current, &latest.version, force, false) {
                println!("codegraph {current} is already up to date");
                return Ok(());
            }
            latest_update_tag(&latest.version)
        }
    };

    let mut builder = configure();
    builder.target_version_tag(&target_tag);
    let updater = builder
        .build()
        .context("configuring the self-update backend")?;

    let status = updater.update().context("performing the self-update")?;
    if status.updated() {
        println!("Updated codegraph to {}", status.version());
    } else {
        println!("codegraph {} is already up to date", status.version());
    }
    Ok(())
}

fn cmd_prompt_hook(path: Option<PathBuf>, query: Option<String>) -> Result<()> {
    let query = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).ok();
            buf
        }
    };
    let query = query.trim();
    if query.is_empty() {
        println!("[codegraph] No query provided — nothing to explore.");
        return Ok(());
    }

    let start = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    let project = resolve_project_path_optional(&start);
    if !is_initialized(&project) {
        println!(
            "[codegraph] No .codegraph index found near {} — run `codegraph init` to enable context.",
            start.display()
        );
        return Ok(());
    }

    let engine = match codegraph_mcp::CodeGraphEngine::open(&project) {
        Ok(engine) => engine,
        Err(err) => {
            println!(
                "[codegraph] Could not open the index at {}: {err}",
                project.display()
            );
            return Ok(());
        }
    };
    let result = engine.execute("codegraph_explore", &json!({ "query": query }));
    for content in &result.content {
        println!("{}", content.text);
    }
    Ok(())
}

fn cmd_init(path: Option<PathBuf>, target: &str) -> Result<()> {
    let project = absolute_path(path.unwrap_or_else(|| PathBuf::from(".")));
    if is_initialized(&project) {
        println!("Already initialized in {}", project.display());
        println!("Use \"codegraph index\" to re-index or \"codegraph sync\" to update");
        return installer::run_install_local_targets(project, target);
    }
    guard_indexable_root(&project)?;
    fs::create_dir_all(codegraph_dir(&project))
        .with_context(|| format!("creating {}", codegraph_dir(&project).display()))?;
    let result = index_project(&project, true, false)?;
    println!("Initialized in {}", project.display());
    print_index_result(&result);
    installer::run_install_local_targets(project, target)
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
    guard_indexable_root(&project)?;
    if force {
        remove_db_files(&project)?;
    }
    let result = index_project_inner(&project, true, verbose, quiet)?;
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
    if !quiet {
        eprintln!("Scanning files…");
    }
    let bar = spinner(
        quiet,
        "{spinner:.green} Syncing {pos}/{len} files ({elapsed})",
    );
    let mut bar_len_set = false;
    let outcome = codegraph_watch::sync_project_once_with_progress(&project, |done, total| {
        if !bar_len_set {
            bar.set_length(total as u64);
            bar_len_set = true;
        }
        bar.set_position(done as u64);
    })?;
    finish_phase(&bar, "Synced files");
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
    let db = db_path(&project);
    let db_exists = db.is_file();
    let daemon_running = daemon_already_running(&project);
    let daemon_pid_path = codegraph_daemon::daemon_pid_path(&project);
    let daemon_socket_path = codegraph_daemon::recorded_socket_path(&project);
    let daemon_log_path = codegraph_daemon::daemon_log_path(&project);
    if !is_initialized(&project) {
        if json_output {
            print_json(&json!({
                "initialized": false,
                "version": VERSION,
                "projectPath": project,
                "indexPath": codegraph_dir(&project),
                "lastIndexed": null,
                "dbPath": db,
                "dbExists": db_exists,
                "daemonRunning": daemon_running,
                "daemonPidPath": daemon_pid_path,
                "daemonSocketPath": daemon_socket_path,
                "daemonLogPath": daemon_log_path,
            }))?;
        } else {
            println!("\nCodeGraph Status\n");
            println!("Project: {}", project.display());
            println!("DB Path: {}", db.display());
            println!(
                "Daemon:  {}",
                if daemon_running { "running" } else { "stopped" }
            );
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
        && built_with_extraction_version.is_none_or(|v| v < EXTRACTION_VERSION);

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
            "dbPath": db,
            "dbExists": db_exists,
            "daemonRunning": daemon_running,
            "daemonPidPath": daemon_pid_path,
            "daemonSocketPath": daemon_socket_path,
            "daemonLogPath": daemon_log_path,
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
    println!("  DB Path:   {}", db.display());
    println!(
        "  Daemon:    {}\n",
        if daemon_running { "running" } else { "stopped" }
    );
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
    let mut results = search_nodes(
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
    if results.iter().all(|r| r.node.name != search) {
        if let Some(resolved) = resolve_gdscript_class_member(&store, &search)? {
            results = resolved
                .into_iter()
                .map(|node| SearchResult { node, score: 1.0 })
                .collect();
        }
    }
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
    language: Option<String>,
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
    if let Some(language) = language {
        files.retain(|f| f.language.as_str() == language);
    }
    if let Some(pattern) = pattern {
        files.retain(|f| glob_matches(&pattern, &f.path));
    }
    for file in &mut files {
        file.node_count = store.node_count_by_file_path(&file.path)?;
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

/// Whether `CODEGRAPH_DEBUG` is truthy (`"1"`/`"true"`), gating the
/// `[codegraph debug]` stderr trace lines. Off ⇒ no new output.
fn debug_enabled() -> bool {
    matches!(
        std::env::var("CODEGRAPH_DEBUG").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn emit_serve_startup_debug(
    project_root: &Path,
    explicit_path: bool,
    has_codegraph: bool,
    mode: &ServeMode,
) {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string());
    let db = db_path(project_root);
    eprintln!(
        "[codegraph debug] serve: exe={exe} cwd={cwd} explicit_path={explicit_path} default_project={} db={} db_exists={} has_codegraph_dir={has_codegraph} mode={mode:?}",
        project_root.display(),
        db.display(),
        db.is_file(),
    );
}

fn cmd_serve(
    path: Option<PathBuf>,
    mcp: bool,
    no_watch: bool,
    http: bool,
    http_addr: String,
) -> Result<()> {
    if http && mcp {
        anyhow::bail!(
            "`--mcp` and `--http` are mutually exclusive: `--mcp` serves MCP over stdio, `--http` serves it over streamable-HTTP. Pick one."
        );
    }
    if http {
        return cmd_serve_http(path, &http_addr);
    }
    // Default the MCP project to cwd so `serve --mcp` (no --path, as the
    // installer injects) finds the index of the agent's project root.
    let explicit_path = path.is_some();
    let project = Some(resolve_project_path_optional(&absolute_path(
        path.unwrap_or_else(|| PathBuf::from(".")),
    )));
    if no_watch {
        std::env::set_var("CODEGRAPH_NO_WATCH", "1");
    }
    if mcp {
        let project_root = project.clone().unwrap_or_else(|| PathBuf::from("."));
        // Stop-the-bleed home guard: an IDE (e.g. Kiro) launches `serve --mcp`
        // with CWD=$HOME, which would otherwise spawn a daemon and run a
        // home-wide catch-up sync that pegs a CPU indexing the entire home
        // tree. When the resolved root is too broad ($HOME or filesystem root),
        // serve tools off any existing index but run NO daemon, watcher, or
        // catch-up. A real project nested under $HOME is unaffected.
        if let Some(reason) = codegraph_watch::too_broad_root_reason(&project_root) {
            eprintln!(
                "[CodeGraph MCP] No project root: {reason}. Tools still answer off an existing index if present."
            );
            return serve_direct_no_services(project, &project_root);
        }
        let has_codegraph = codegraph_dir(&project_root).is_dir();
        let mode = select_serve_mode(daemon_opt_out(), is_daemon_internal(), has_codegraph);
        if debug_enabled() {
            emit_serve_startup_debug(&project_root, explicit_path, has_codegraph, &mode);
        }
        match mode {
            ServeMode::Direct => {
                return serve_direct(project, &project_root, no_watch, explicit_path);
            }
            ServeMode::BeDaemon => {
                return codegraph_daemon::run_foreground(
                    &project_root,
                    codegraph_daemon::DaemonOptions {
                        run_mcp: true,
                        ..Default::default()
                    },
                )
                .context("running as detached MCP daemon");
            }
            ServeMode::SpawnOrProxy => {
                if let Some(result) = spawn_or_proxy(project.clone(), &project_root, no_watch) {
                    return result;
                }
                return serve_direct(project, &project_root, no_watch, explicit_path);
            }
        }
    }
    eprintln!("\nCodeGraph daemon/watch server\n");
    eprintln!("Daemon and watcher startup is wired here for tasks 24/25.");
    eprintln!("Use `codegraph serve --mcp` to start the committed MCP stdio server.");
    Ok(())
}

/// `serve --http`: serve MCP over streamable-HTTP (rmcp), pinned to an
/// already-indexed project. Resolves the project from `--path` (find-up if
/// omitted), REQUIRES an on-disk index (hard-errors otherwise — never
/// self-indexes), parses the bind address, and hands off to
/// `codegraph_mcp::serve_http`. Not routed through the daemon/SpawnOrProxy path.
fn cmd_serve_http(path: Option<PathBuf>, http_addr: &str) -> Result<()> {
    let project =
        resolve_project_path_optional(&absolute_path(path.unwrap_or_else(|| PathBuf::from("."))));
    let db = db_path(&project);
    if !db.is_file() {
        anyhow::bail!(
            "`serve --http` requires an indexed project, but no index was found at {}. Run `codegraph init {}` (or `codegraph index`) first, or pass `--path <indexed-project>`.",
            db.display(),
            project.display(),
        );
    }
    let addr: std::net::SocketAddr = http_addr.parse().with_context(|| {
        format!("invalid --http-addr {http_addr:?}: expected an <ip>:<port> socket address")
    })?;
    if debug_enabled() {
        eprintln!(
            "[codegraph debug] serve --http: addr={addr} project={} db={} db_exists={}",
            project.display(),
            db.display(),
            db.is_file(),
        );
    }
    serve_http_impl(project, addr)
}

/// Feature-gated indirection to `codegraph_mcp::serve_http`. Under the `rmcp`
/// feature this forwards to the streamable-HTTP server; the shipped default
/// build is rmcp-free, so the flag exists but errors with an actionable message.
#[cfg(feature = "rmcp")]
fn serve_http_impl(project: PathBuf, addr: std::net::SocketAddr) -> Result<()> {
    codegraph_mcp::serve_http(project, addr).context("serving MCP over streamable-HTTP")
}

#[cfg(not(feature = "rmcp"))]
fn serve_http_impl(_project: PathBuf, _addr: std::net::SocketAddr) -> Result<()> {
    anyhow::bail!(
        "codegraph was built without the `rmcp` feature, so `serve --http` (streamable-HTTP) is unavailable. Rebuild with `--features rmcp` to enable it, or use `serve --mcp` (stdio)."
    )
}

/// Whether `serve --mcp` should start background services (live watcher +
/// catch-up sync) for `project_root`. They run when the path was EXPLICIT
/// (`--path X` — the user opted into X) or the cwd is ALREADY indexed. A bare
/// serve from an UNINDEXED cwd (the Zed case) returns false so catch-up never
/// self-indexes the cwd — keeping it unindexed and therefore adoptable when the
/// client reports its real workspace root via `roots/list`.
fn should_run_serve_services(explicit_path: bool, project_root: &Path) -> bool {
    explicit_path || codegraph_dir(project_root).is_dir()
}

fn serve_direct(
    project: Option<PathBuf>,
    project_root: &Path,
    no_watch: bool,
    explicit_path: bool,
) -> Result<()> {
    let run_services = should_run_serve_services(explicit_path, project_root);
    // Watcher startup stays here (pre-handshake). Layer A
    // (`watch_disabled_reason`) already refuses to walk HOME / the filesystem
    // root, so a home-rooted launch never exhausts inotify. Restarting the
    // watcher against a project root adopted later from the `initialize` roots
    // (Layer B) would require McpServer to own the watcher lifecycle across
    // crates; it is deferred — the adopted root still serves tools and is
    // reconciled by the background catch-up sync, just without a live watch.
    // Skipped entirely for a bare serve from an unindexed cwd so the cwd is
    // never self-indexed (keeps it adoptable via roots/list).
    let _watcher = run_services.then(|| start_direct_watcher(project_root, no_watch));
    // Background catch-up of edits made while the server was down (#905). It runs
    // on a detached worker thread; `server.run` proceeds immediately so the FIRST
    // tools/call NEVER waits on the reconcile. Bind the flag to keep it alive (a
    // future status surface can read it); it is intentionally never awaited.
    // Skipped for a too-broad root ($HOME / filesystem root) — `sync_project_once`
    // there walks the entire home tree and pegs a CPU at 99% — and for a bare
    // serve from an unindexed cwd, where `Store::open` would otherwise create
    // `.codegraph/` and race roots adoption (the real project root Zed reports
    // would then be rejected as "already indexed cwd").
    let _catch_up_done = (run_services && should_run_daemon_services(project_root))
        .then(|| spawn_catch_up(project_root));
    serve_direct_stdio(project)
}

/// Serve the direct (pinned) stdio path. Under `--features rmcp` with
/// `CODEGRAPH_DAEMON_RMCP=1` this drives the rmcp [`CodeGraphHandler`] (Phase D);
/// otherwise it drives the hand-rolled [`McpServer`]. Both block until stdin
/// EOF. The broad-root/unindexed-cwd adoption handoff keeps the hand-rolled
/// path (`serve_direct_no_services`), since rmcp owns its read loop and cannot
/// hand the reader back for the daemon proxy.
#[cfg(feature = "rmcp")]
fn serve_direct_stdio(project: Option<PathBuf>) -> Result<()> {
    if std::env::var("CODEGRAPH_DAEMON_RMCP").as_deref() == Ok("1") {
        return codegraph_mcp::serve_stdio_rmcp(project).context("running rmcp MCP stdio server");
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut server = McpServer::new(project);
    server
        .run(BufReader::new(stdin.lock()), stdout.lock())
        .context("running MCP stdio server")
}

#[cfg(not(feature = "rmcp"))]
fn serve_direct_stdio(project: Option<PathBuf>) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut server = McpServer::new(project);
    server
        .run(BufReader::new(stdin.lock()), stdout.lock())
        .context("running MCP stdio server")
}

/// Serves MCP tools off any existing index WITHOUT starting the watcher,
/// daemon, or catch-up sync. Used when the resolved root is too broad
/// ($HOME / filesystem root), where background services would index the whole
/// home tree.
fn serve_direct_no_services(project: Option<PathBuf>, _project_root: &Path) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut server = McpServer::new(project);
    match server
        .run_until_adoption(BufReader::new(stdin.lock()), stdout.lock())
        .context("running MCP stdio server until workspace adoption")?
    {
        RunUntilAdoption::Eof => Ok(()),
        RunUntilAdoption::Adopted {
            project_root,
            reader,
        } => serve_adopted_project(reader, stdout, project_root),
    }
}

fn serve_adopted_project<R: BufRead, W: Write + Send + 'static>(
    reader: R,
    writer: W,
    project_root: PathBuf,
) -> Result<()> {
    let Some(socket_path) = start_daemon_for_adopted_root(&project_root) else {
        let mut server = McpServer::new(Some(project_root));
        return server
            .run(reader, writer)
            .context("running MCP stdio server for adopted project");
    };

    match codegraph_daemon::attach_to_daemon(&socket_path) {
        Ok(client) if codegraph_daemon::verify_daemon_hello(&client.hello).is_none() => {}
        Ok(_) => {
            tracing::debug!("adopted project daemon version mismatch; serving direct");
            if debug_enabled() {
                eprintln!(
                    "[codegraph debug] serve_adopted: daemon version mismatch; serving direct"
                );
            }
            let mut server = McpServer::new(Some(project_root));
            return server
                .run(reader, writer)
                .context("running MCP stdio server for adopted project");
        }
        Err(err) => {
            tracing::debug!(error = %err, "adopted project daemon preflight failed; serving direct");
            if debug_enabled() {
                eprintln!(
                    "[codegraph debug] serve_adopted: daemon preflight failed ({err}); serving direct"
                );
            }
            let mut server = McpServer::new(Some(project_root));
            return server
                .run(reader, writer)
                .context("running MCP stdio server for adopted project");
        }
    }

    match codegraph_daemon::run_proxy(
        &socket_path,
        Some(codegraph_daemon::current_ppid()),
        reader,
        writer,
    ) {
        Ok(codegraph_daemon::ProxyOutcome::Proxied) => Ok(()),
        Ok(codegraph_daemon::ProxyOutcome::VersionMismatch) => Ok(()),
        Err(err) => {
            tracing::debug!(error = %err, "adopted project proxy attach failed");
            if debug_enabled() {
                eprintln!("[codegraph debug] serve_adopted: proxy attach failed ({err})");
            }
            Ok(())
        }
    }
}

fn start_daemon_for_adopted_root(project_root: &Path) -> Option<PathBuf> {
    if daemon_opt_out() || is_daemon_internal() || !should_run_daemon_services(project_root) {
        return None;
    }
    if !codegraph_dir(project_root).is_dir() {
        return None;
    }
    if daemon_already_running(project_root) {
        if debug_enabled() {
            eprintln!(
                "[codegraph debug] adopted-root: attaching to existing daemon (pid_path={} socket_path={})",
                codegraph_daemon::daemon_pid_path(project_root).display(),
                codegraph_daemon::recorded_socket_path(project_root).display(),
            );
        }
        return Some(codegraph_daemon::recorded_socket_path(project_root));
    }
    let Ok(exe) = std::env::current_exe() else {
        return None;
    };
    match codegraph_daemon::spawn_detached_daemon(&exe, project_root) {
        Ok(()) => {
            poll_for_daemon_socket(project_root);
            eprintln!(
                "[CodeGraph MCP] Started shared daemon for adopted project root {}",
                project_root.display()
            );
            if debug_enabled() {
                eprintln!(
                    "[codegraph debug] adopted-root: spawned new daemon (pid_path={} socket_path={})",
                    codegraph_daemon::daemon_pid_path(project_root).display(),
                    codegraph_daemon::recorded_socket_path(project_root).display(),
                );
            }
            let socket_path = codegraph_daemon::recorded_socket_path(project_root);
            socket_path.exists().then_some(socket_path)
        }
        Err(err) => {
            eprintln!("[CodeGraph MCP] Adopted project daemon start failed: {err}");
            None
        }
    }
}

/// Whether daemon-style background services (detached daemon, file watcher,
/// catch-up sync) may run against `root`. Returns `false` for a too-broad root
/// ($HOME or the filesystem root); shares the decision with the watcher guard
/// via `codegraph_watch::too_broad_root_reason`.
fn should_run_daemon_services(root: &Path) -> bool {
    codegraph_watch::too_broad_root_reason(root).is_none()
}

fn guard_indexable_root(root: &Path) -> Result<()> {
    if let Some(reason) = codegraph_watch::too_broad_root_reason(root) {
        bail!(
            "refusing to index {}: {reason}. Run `codegraph init`/`index` inside a specific project directory instead.",
            root.display()
        );
    }
    Ok(())
}

/// Spawn a ONE-SHOT background catch-up sync that absorbs edits made while the
/// server was down (upstream colby `catchUpSync`, #905). Returns an
/// `Arc<AtomicBool>` flipped to `true` when the background sync finishes, so a
/// status surface could observe completion. The request path MUST NOT block on
/// it: this runs on a detached `std::thread` and is never joined on the
/// handshake / tool-call path.
fn spawn_catch_up(project_root: &Path) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let root = project_root.to_path_buf();
    std::thread::spawn(move || {
        match codegraph_watch::sync_project_once(&root) {
            Ok(outcome) => {
                let changed = outcome.files_reindexed + outcome.files_removed;
                if changed > 0 {
                    eprintln!("[CodeGraph MCP] Caught up {changed} file(s) changed since last run");
                }
            }
            Err(err) => {
                eprintln!("[CodeGraph MCP] Catch-up sync failed: {err}");
            }
        }
        thread_done.store(true, Ordering::SeqCst);
    });
    done
}

fn start_direct_watcher(
    project_root: &Path,
    no_watch: bool,
) -> Option<codegraph_watch::ProjectWatcher> {
    let mut opts = codegraph_watch::WatchOptions::default();
    opts.no_watch = no_watch;
    opts.on_sync_complete = Some(std::sync::Arc::new(
        |outcome: codegraph_watch::SyncOutcome| {
            eprintln!(
                "[CodeGraph MCP] Auto-synced {} file(s) in {}ms",
                outcome.files_reindexed, outcome.duration_ms
            );
        },
    ));
    opts.on_degraded = Some(std::sync::Arc::new(|reason: String| {
        eprintln!("[CodeGraph MCP] File watcher degraded — {reason}");
    }));
    opts.on_sync_error = Some(std::sync::Arc::new(|reason: String| {
        eprintln!("[CodeGraph MCP] File watcher warning — {reason}");
    }));
    match codegraph_watch::start_serve_watcher(project_root, opts) {
        Ok(Some(watcher)) => {
            eprintln!("[CodeGraph MCP] File watcher active — graph will auto-sync on changes");
            Some(watcher)
        }
        Ok(None) => {
            let reason = codegraph_watch::watch_disabled_reason(project_root, no_watch)
                .unwrap_or_else(|| "watching disabled".to_string());
            eprintln!("[CodeGraph MCP] File watcher disabled — {reason}");
            None
        }
        Err(err) => {
            eprintln!("[CodeGraph MCP] File watcher failed to start: {err}");
            None
        }
    }
}

const DAEMON_SOCKET_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const DAEMON_SOCKET_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// Spawn the shared daemon if needed, poll for its socket, then run the real
/// proxy. Returns `Some(Ok(()))` when the proxy bridged the session (caller
/// must NOT also serve direct), or `None` when the proxy could not attach
/// (daemon spawn failed, socket never appeared, or a version mismatch) — the
/// caller then transparently falls back to direct serving.
fn spawn_or_proxy(
    _project: Option<PathBuf>,
    project_root: &Path,
    _no_watch: bool,
) -> Option<Result<()>> {
    let dbg = debug_enabled();
    if dbg {
        eprintln!(
            "[codegraph debug] spawn_or_proxy: pid_path={} socket_path={}",
            codegraph_daemon::daemon_pid_path(project_root).display(),
            codegraph_daemon::recorded_socket_path(project_root).display(),
        );
    }
    if daemon_already_running(project_root) {
        if dbg {
            eprintln!("[codegraph debug] spawn_or_proxy: attaching to existing daemon");
        }
    } else {
        match std::env::current_exe() {
            Ok(exe) => {
                if let Err(err) = codegraph_daemon::spawn_detached_daemon(&exe, project_root) {
                    tracing::debug!(error = %err, "detached daemon spawn failed; serving direct");
                    if dbg {
                        eprintln!(
                            "[codegraph debug] spawn_or_proxy: daemon spawn failed ({err}); falling back to direct"
                        );
                    }
                    return None;
                }
                if dbg {
                    eprintln!("[codegraph debug] spawn_or_proxy: spawned new daemon");
                }
                poll_for_daemon_socket(project_root);
            }
            Err(err) => {
                tracing::debug!(error = %err, "current_exe unavailable; serving direct");
                if dbg {
                    eprintln!(
                        "[codegraph debug] spawn_or_proxy: current_exe unavailable ({err}); falling back to direct"
                    );
                }
                return None;
            }
        }
    }

    let socket_path = codegraph_daemon::recorded_socket_path(project_root);
    if !socket_path.exists() {
        tracing::debug!("daemon socket never appeared; serving direct");
        if dbg {
            eprintln!(
                "[codegraph debug] spawn_or_proxy: daemon socket never appeared; falling back to direct"
            );
        }
        return None;
    }

    let host_ppid = Some(codegraph_daemon::current_ppid());
    let stdin = io::stdin();
    match codegraph_daemon::run_proxy(
        &socket_path,
        host_ppid,
        BufReader::new(stdin.lock()),
        io::stdout(),
    ) {
        Ok(codegraph_daemon::ProxyOutcome::Proxied) => Some(Ok(())),
        Ok(codegraph_daemon::ProxyOutcome::VersionMismatch) => {
            tracing::debug!("daemon version mismatch; serving direct");
            if dbg {
                eprintln!(
                    "[codegraph debug] spawn_or_proxy: daemon version mismatch; falling back to direct"
                );
            }
            None
        }
        Err(err) => {
            tracing::debug!(error = %err, "proxy attach failed; serving direct");
            if dbg {
                eprintln!(
                    "[codegraph debug] spawn_or_proxy: proxy attach failed ({err}); falling back to direct"
                );
            }
            None
        }
    }
}

fn daemon_already_running(project_root: &Path) -> bool {
    let pid_path = codegraph_daemon::daemon_pid_path(project_root);
    let Ok(raw) = fs::read_to_string(&pid_path) else {
        return false;
    };
    codegraph_daemon::decode_lock_info(&raw)
        .filter(|info| info.pid > 0)
        .is_some_and(|info| codegraph_daemon::is_process_alive(info.pid))
}

fn poll_for_daemon_socket(project_root: &Path) {
    let deadline = std::time::Instant::now() + DAEMON_SOCKET_POLL_TIMEOUT;
    while std::time::Instant::now() < deadline {
        // Re-read the lock each tick: the daemon rewrites the recorded socket to
        // its bind-fallback choice during startup, so the path can change while
        // we poll (D-Daemon-b).
        if codegraph_daemon::recorded_socket_path(project_root).exists() {
            return;
        }
        std::thread::sleep(DAEMON_SOCKET_POLL_INTERVAL);
    }
}

fn daemon_opt_out() -> bool {
    std::env::var(codegraph_daemon::CODEGRAPH_NO_DAEMON).as_deref() == Ok("1")
}

fn is_daemon_internal() -> bool {
    std::env::var(codegraph_daemon::CODEGRAPH_DAEMON_INTERNAL).as_deref() == Ok("1")
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServeMode {
    Direct,
    BeDaemon,
    SpawnOrProxy,
}

pub fn select_serve_mode(
    no_daemon: bool,
    is_daemon_internal: bool,
    has_codegraph: bool,
) -> ServeMode {
    if no_daemon {
        ServeMode::Direct
    } else if is_daemon_internal {
        ServeMode::BeDaemon
    } else if !has_codegraph {
        ServeMode::Direct
    } else {
        ServeMode::SpawnOrProxy
    }
}

#[cfg(test)]
mod serve_mode_tests {
    use super::{
        debug_enabled, guard_indexable_root, select_serve_mode, should_run_daemon_services,
        should_run_serve_services, ServeMode,
    };
    use std::path::Path;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn debug_enabled_honors_truthy_values_only() {
        let _lock = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CODEGRAPH_DEBUG").ok();

        std::env::remove_var("CODEGRAPH_DEBUG");
        assert!(!debug_enabled(), "unset ⇒ off");
        std::env::set_var("CODEGRAPH_DEBUG", "1");
        assert!(debug_enabled(), "\"1\" ⇒ on");
        std::env::set_var("CODEGRAPH_DEBUG", "true");
        assert!(debug_enabled(), "\"true\" ⇒ on");
        std::env::set_var("CODEGRAPH_DEBUG", "0");
        assert!(!debug_enabled(), "\"0\" ⇒ off");
        std::env::set_var("CODEGRAPH_DEBUG", "yes");
        assert!(!debug_enabled(), "any other value ⇒ off");

        match prev {
            Some(v) => std::env::set_var("CODEGRAPH_DEBUG", v),
            None => std::env::remove_var("CODEGRAPH_DEBUG"),
        }
    }

    #[test]
    fn select_serve_mode_decision_order() {
        assert_eq!(select_serve_mode(true, false, true), ServeMode::Direct);
        assert_eq!(select_serve_mode(false, true, true), ServeMode::BeDaemon);
        assert_eq!(select_serve_mode(false, false, false), ServeMode::Direct);
        assert_eq!(
            select_serve_mode(false, false, true),
            ServeMode::SpawnOrProxy
        );
    }

    #[test]
    fn serve_services_gate_skips_unindexed_bare_cwd_but_runs_when_explicit_or_indexed() {
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let unindexed =
            std::env::temp_dir().join(format!("cg-serve-gate-unidx-{}-{seq}", std::process::id()));
        let indexed =
            std::env::temp_dir().join(format!("cg-serve-gate-idx-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&unindexed).unwrap();
        std::fs::create_dir_all(indexed.join(".codegraph")).unwrap();

        assert!(
            should_run_serve_services(true, &unindexed),
            "explicit --path must run services even on an unindexed root"
        );
        assert!(
            !should_run_serve_services(false, &unindexed),
            "bare serve from an unindexed cwd must NOT run services (keeps cwd adoptable)"
        );
        assert!(
            should_run_serve_services(false, &indexed),
            "an already-indexed cwd must keep services"
        );

        let _ = std::fs::remove_dir_all(&unindexed);
        let _ = std::fs::remove_dir_all(&indexed);
    }

    #[test]
    fn daemon_services_disabled_at_home_and_root_enabled_for_nested_project() {
        let _lock = ENV_LOCK.lock().unwrap();
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let prev_home = std::env::var_os(home_key);

        let tmp = std::env::temp_dir().join(format!("cg-serve-home-{}", std::process::id()));
        let nested = tmp.join("workspace/ProdDir/AI/codegraph-rust");
        std::fs::create_dir_all(&nested).unwrap();
        std::env::set_var(home_key, &tmp);

        assert!(
            !should_run_daemon_services(&tmp),
            "$HOME must disable daemon services"
        );
        assert!(
            !should_run_daemon_services(Path::new("/")),
            "filesystem root must disable daemon services"
        );
        assert!(
            should_run_daemon_services(&nested),
            "a project nested under $HOME must keep daemon services"
        );

        match prev_home {
            Some(v) => std::env::set_var(home_key, v),
            None => std::env::remove_var(home_key),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn guard_indexable_root_rejects_home_and_root_allows_nested_project() {
        let _lock = ENV_LOCK.lock().unwrap();
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let prev_home = std::env::var_os(home_key);

        let tmp = std::env::temp_dir().join(format!("cg-guard-home-{}", std::process::id()));
        let nested = tmp.join("workspace/proj");
        std::fs::create_dir_all(&nested).unwrap();
        std::env::set_var(home_key, &tmp);

        assert!(
            guard_indexable_root(&tmp).is_err(),
            "$HOME must be refused as an index root"
        );
        assert!(
            guard_indexable_root(Path::new("/")).is_err(),
            "filesystem root must be refused as an index root"
        );
        assert!(
            guard_indexable_root(&nested).is_ok(),
            "a project nested under $HOME must be indexable"
        );

        match prev_home {
            Some(v) => std::env::set_var(home_key, v),
            None => std::env::remove_var(home_key),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
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
    let godot = godot_honesty_for_symbol(&store, &project, &symbol)?;
    if json_output {
        print_json_pretty(&json!({
            "symbol": symbol,
            "callers": nodes,
            "godotDynamic": godot.as_json(),
        }))?;
    } else {
        print_related("Callers", &symbol, &nodes);
        godot.print_cli(nodes.is_empty());
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
    let godot = godot_honesty_for_symbol(&store, &project, &symbol)?;
    if json_output {
        print_json_pretty(&json!({
            "symbol": symbol,
            "depth": depth,
            "nodeCount": affected.len(),
            "edgeCount": edge_keys.len(),
            "affected": affected,
            "godotDynamic": godot.as_json(),
        }))?;
    } else {
        println!(
            "\nImpact of changing \"{}\" - {} affected symbols:\n",
            symbol,
            affected.len()
        );
        print_by_file(&affected);
        godot.print_cli(affected.is_empty());
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

fn audit_prefix_keep(path: &str, include: &[String], exclude: &[String]) -> bool {
    let normalized = path.replace('\\', "/");
    let under = |prefix: &String| normalized.starts_with(&prefix.replace('\\', "/"));
    if !include.is_empty() && !include.iter().any(under) {
        return false;
    }
    !exclude.iter().any(under)
}

struct AuditArgs {
    path: Option<PathBuf>,
    orphans: bool,
    dangling: bool,
    impact: Option<String>,
    verify_plan: bool,
    include: Vec<String>,
    exclude: Vec<String>,
    json_output: bool,
}

fn cmd_audit(args: AuditArgs) -> Result<()> {
    let AuditArgs {
        path,
        orphans,
        dangling,
        impact,
        verify_plan,
        include,
        exclude,
        json_output,
    } = args;
    if !orphans && !dangling && impact.is_none() {
        bail!("audit requires at least one of --orphans, --dangling, --impact <path>");
    }
    let project = resolve_required_project(path)?;
    let store = open_store(&project)?;
    let traverser = GraphTraverser::new(&store);

    let mut orphan_list = if orphans {
        traverser.find_orphan_resources()?
    } else {
        Vec::new()
    };
    orphan_list.retain(|o| audit_prefix_keep(&o.file_path, &include, &exclude));
    let mut dangling_list = if dangling {
        traverser.find_dangling_references(&project)?
    } else {
        Vec::new()
    };
    dangling_list.retain(|d| audit_prefix_keep(&d.from_file, &include, &exclude));
    let impact_result = match &impact {
        Some(changed) => {
            let normalized = normalize_impact_input(changed, &project);
            let mut result = traverser.resource_impact(&normalized)?;
            result
                .affected
                .retain(|a| audit_prefix_keep(&a.from_file, &include, &exclude));
            Some(result)
        }
        None => None,
    };

    if json_output {
        let mut out = serde_json::Map::new();
        if orphans {
            out.insert("orphans".to_string(), json!(orphan_list));
        }
        if dangling {
            out.insert("dangling".to_string(), json!(dangling_list));
        }
        if let Some(result) = &impact_result {
            out.insert("impact".to_string(), json!(result));
            if let Some(note) = empty_impact_note(result) {
                out.insert("note".to_string(), json!(note));
            }
            if verify_plan {
                out.insert("verifyPlan".to_string(), json!(verify_plan_view(result)));
            }
        }
        print_json_pretty(&serde_json::Value::Object(out))?;
        return Ok(());
    }

    if orphans {
        print_audit_orphans(&orphan_list);
    }
    if dangling {
        print_audit_dangling(&dangling_list);
    }
    if let Some(result) = &impact_result {
        print_audit_impact(result);
        if verify_plan {
            print_verify_plan(&verify_plan_view(result));
        }
    }
    Ok(())
}

/// Derived load/open plan for one impact result: the `.gd` scripts to reload and
/// `.tscn` scenes to reopen that reference the changed path, plus per-site reasons.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPlan {
    changed: String,
    load_scripts: Vec<String>,
    load_resources: Vec<String>,
    open_scenes: Vec<String>,
    reasons: Vec<VerifyReason>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyReason {
    file: String,
    line: i64,
    edge_kind: String,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_subkind: Option<String>,
}

/// Normalize a raw `audit --impact <changed>` value into the project-relative,
/// `/`-separated form that `resource_impact` expects. Strict order: strip a
/// leading `res://` FIRST (so a `res://…` value is never mistaken for an
/// absolute path), then a leading `./` or `.\`, then convert `\` to `/`. If the
/// result is an OS-absolute path under the project root, make it relative; an
/// absolute path outside the root passes through unchanged (yields an empty
/// impact rather than an error).
fn normalize_impact_input(changed: &str, project: &Path) -> String {
    let mut s = changed;
    if let Some(rest) = s.strip_prefix("res://") {
        s = rest;
    }
    if let Some(rest) = s.strip_prefix("./").or_else(|| s.strip_prefix(".\\")) {
        s = rest;
    }
    let s = s.replace('\\', "/");
    let candidate = Path::new(&s);
    if candidate.is_absolute() {
        if let Ok(rel) = candidate.strip_prefix(project) {
            return rel.to_string_lossy().replace('\\', "/");
        }
    }
    s
}

fn verify_plan_view(impact: &codegraph_graph::graph::ResourceImpact) -> VerifyPlan {
    let mut load_scripts: Vec<String> = Vec::new();
    let mut load_resources: Vec<String> = Vec::new();
    let mut open_scenes: Vec<String> = Vec::new();
    let mut reasons: Vec<VerifyReason> = Vec::new();
    if impact.changed.ends_with(".gd") {
        load_scripts.push(res_path(&impact.changed));
    } else if impact.changed.ends_with(".tres") || impact.changed.ends_with(".res") {
        load_resources.push(res_path(&impact.changed));
    } else if impact.changed.ends_with(".tscn") {
        open_scenes.push(res_path(&impact.changed));
    }
    for affected in &impact.affected {
        if affected.from_file.ends_with(".gd") {
            load_scripts.push(res_path(&affected.from_file));
        } else if affected.from_file.ends_with(".tres") || affected.from_file.ends_with(".res") {
            load_resources.push(res_path(&affected.from_file));
        } else if affected.from_file.ends_with(".tscn") {
            open_scenes.push(res_path(&affected.from_file));
        }
        reasons.push(VerifyReason {
            file: affected.from_file.clone(),
            line: affected.line,
            edge_kind: affected.edge_kind.clone(),
            target: affected.target.clone(),
            edge_subkind: affected.edge_subkind.clone(),
        });
    }
    load_scripts.sort();
    load_scripts.dedup();
    load_resources.sort();
    load_resources.dedup();
    open_scenes.sort();
    open_scenes.dedup();
    VerifyPlan {
        changed: impact.changed.clone(),
        load_scripts,
        load_resources,
        open_scenes,
        reasons,
    }
}

fn res_path(rel: &str) -> String {
    format!("res://{}", rel.replace('\\', "/"))
}

/// The static boundary note for an EMPTY impact on a Godot resource/script path:
/// "nothing references X" is not proof of zero use (data-driven numeric-id/DSL
/// refs are not followed). `None` when the impact is non-empty or the path is not
/// a Godot resource/script.
fn empty_impact_note(impact: &codegraph_graph::graph::ResourceImpact) -> Option<String> {
    if !impact.affected.is_empty() {
        return None;
    }
    if !is_godot_resource_path(&impact.changed) {
        return None;
    }
    Some(
        "no static references found; godot data-driven numeric-id/DSL references are not included by default"
            .to_string(),
    )
}

fn is_godot_resource_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".tres")
        || lower.ends_with(".tscn")
        || lower.ends_with(".res")
        || lower.ends_with(".gd")
}

fn print_audit_orphans(orphans: &[codegraph_graph::graph::OrphanResource]) {
    if orphans.is_empty() {
        println!("No orphan resources found");
    } else {
        println!("\nFound {} orphan resources:\n", orphans.len());
        for orphan in orphans {
            print!("  {} [{}]", orphan.file_path, orphan.confidence);
            if let Some(note) = &orphan.note {
                print!(" \u{2014} {note}");
            }
            println!();
        }
    }
}

fn print_audit_dangling(dangling: &[codegraph_graph::graph::DanglingRef]) {
    if dangling.is_empty() {
        println!("No dangling references found");
    } else {
        println!("\nFound {} dangling references:\n", dangling.len());
        for reference in dangling {
            println!(
                "  {}:{} \u{2192} {} ({})",
                reference.from_file, reference.line, reference.target_path, reference.kind
            );
        }
    }
}

fn print_audit_impact(impact: &codegraph_graph::graph::ResourceImpact) {
    if impact.affected.is_empty() {
        println!("\nNothing references {}", impact.changed);
        if let Some(note) = empty_impact_note(impact) {
            println!("  note: {note}");
        }
    } else {
        println!(
            "\n{} is referenced by {} site(s):\n",
            impact.changed,
            impact.affected.len()
        );
        for affected in &impact.affected {
            match &affected.edge_subkind {
                Some(subkind) => println!(
                    "  {}:{} ({}/{})",
                    affected.from_file, affected.line, affected.edge_kind, subkind
                ),
                None => println!(
                    "  {}:{} ({})",
                    affected.from_file, affected.line, affected.edge_kind
                ),
            }
        }
    }
}

fn print_verify_plan(plan: &VerifyPlan) {
    println!("\nverify-plan for {}:", plan.changed);
    println!("  loadScripts ({}):", plan.load_scripts.len());
    for script in &plan.load_scripts {
        println!("    {script}");
    }
    println!("  loadResources ({}):", plan.load_resources.len());
    for resource in &plan.load_resources {
        println!("    {resource}");
    }
    println!("  openScenes ({}):", plan.open_scenes.len());
    for scene in &plan.open_scenes {
        println!("    {scene}");
    }
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

// Progress is a pure side effect: it only counts/displays and never gates,
// reorders, or alters extraction, so golden byte-equivalence is preserved. It
// draws to stderr (stdout carries JSON / golden output) and is hidden when
// stderr is not a TTY or `--quiet`, so CI logs and pipes stay clean.
fn progress_bar(len: u64, quiet: bool, template: &str) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(Some(len), ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template(template) {
        bar.set_style(style.progress_chars("=>-"));
    }
    bar
}

fn spinner(quiet: bool, template: &str) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template(template) {
        bar.set_style(style);
    }
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}

// A labeled phase spinner that ticks while running. `finish_phase` retains a
// "✓ <label> (<elapsed>)" summary line on stderr (vs finish_and_clear which
// wipes it); gated like the other indicators.
fn phase_spinner(label: &str, quiet: bool) -> ProgressBar {
    if quiet || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    if let Ok(style) = ProgressStyle::with_template("{spinner:.green} {msg}") {
        bar.set_style(style);
    }
    bar.set_message(label.to_string());
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}

fn finish_phase(bar: &ProgressBar, label: &str) {
    if bar.is_hidden() {
        return;
    }
    let elapsed = format_duration(bar.elapsed().as_millis() as i64);
    if let Ok(style) = ProgressStyle::with_template("{msg}") {
        bar.set_style(style);
    }
    bar.abandon_with_message(format!("✓ {label} ({elapsed})"));
}

fn index_project(project: &Path, clear_first: bool, verbose: bool) -> Result<IndexSummary> {
    index_project_inner(project, clear_first, verbose, false)
}

/// Restores the shared `synchronous=NORMAL` durability (and truncates the WAL) when
/// the full index finishes OR bails out early via `?`. Drop never panics: a failed
/// restore is logged, not propagated.
struct BulkIndexPragmaGuard {
    db_path: PathBuf,
}

impl Drop for BulkIndexPragmaGuard {
    fn drop(&mut self) {
        let result = match Store::open(&self.db_path) {
            Ok(store) => store.restore_default_pragmas().map_err(anyhow::Error::from),
            Err(err) => Err(anyhow::Error::from(err)),
        };
        if let Err(err) = result {
            tracing::warn!(
                error = %err,
                db = %self.db_path.display(),
                "failed to restore default pragmas after full index",
            );
        }
    }
}

fn index_project_inner(
    project: &Path,
    clear_first: bool,
    verbose: bool,
    quiet: bool,
) -> Result<IndexSummary> {
    let started = std::time::Instant::now();
    if clear_first {
        remove_db_files(project)?;
    }
    fs::create_dir_all(codegraph_dir(project))?;
    let config = codegraph_core::config::get_config();
    let options = ExtractOptions {
        max_file_size: config.indexing.max_file_size,
        ignore_dirs: config.indexing.ignore_dirs.clone(),
        exclude: config.indexing.exclude.clone(),
        parallel: true,
    };
    if !quiet {
        eprintln!("Scanning files…");
    }
    let files = codegraph_extract::engine::scan_project(project, &options)?;

    // `synchronous=OFF` + a larger cache/mmap window speed up the from-scratch bulk
    // index. The restore lives in a Drop guard, NOT a trailing statement, because
    // every `?` below would skip a trailing restore and leave `synchronous=OFF`
    // durable on the error path. Declared BEFORE `store` so it drops AFTER it: the
    // guard's own connection then runs wal_checkpoint(TRUNCATE)+NORMAL with no WAL
    // contention, leaving the file in the same shape a NORMAL run produces.
    let _pragma_guard = BulkIndexPragmaGuard {
        db_path: db_path(project),
    };
    let store = open_store(project)?;
    store.set_bulk_index_pragmas()?;

    let before = store.counts()?;
    let files_indexed = 0;
    let files_skipped = 0;
    let files_errored = 0;

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

    let spill = SpillWriter::new(codegraph_dir(project))?;
    let pending_nodes: Vec<Node> = Vec::with_capacity(NODE_FLUSH_ROWS);

    let bar = progress_bar(
        files.len() as u64,
        quiet,
        "{spinner:.green} Indexing [{bar:30}] {pos}/{len} files ({elapsed}) {wide_msg}",
    );
    if verbose {
        bar.set_message(format!(
            "parsing ({} threads)",
            rayon::current_num_threads()
        ));
    }

    // Overlap parse (rayon producers) with persist (one ordered consumer) while
    // persisting in EXACT sorted `scan_project` order — byte-identical to the
    // serial drain. The handoff channel is UNBOUNDED so a producer's `send` never
    // parks a rayon worker; memory is bounded by a reorder WINDOW instead: a
    // producer for index `i` waits until `i < next_expected + WINDOW`, capping the
    // buffer to ≤ WINDOW entries. The head index (`i == next_expected`) is never
    // gated, so the consumer can always advance — deadlock-free by construction.
    // Producers only parse; the consumer alone touches the Store.
    const PARSE_REORDER_WINDOW: usize = 512;

    type ParsePayload = (usize, String, FileRecord, ExtractionResult);
    let (tx, rx) = mpsc::channel::<ParsePayload>();
    let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
    // Set on a consumer store-write error so gated producers wake and abort.
    let abort = Arc::new(AtomicBool::new(false));

    let producer_err: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));
    let bar_for_finish = bar.clone();

    // The scope closure is `move` so it owns `rx` (a `Receiver` is `Send` but not
    // `Sync`, so it cannot be captured by reference into the `Send` closure). The
    // consumer-side owned state (store/spill/pending_nodes/counters) moves in too
    // and is returned out so the rest of the function can continue using it.
    let (
        consumer_err,
        mut store,
        spill,
        pending_nodes,
        files_indexed,
        files_skipped,
        files_errored,
    ) = {
        let gate = Arc::clone(&gate);
        let abort = Arc::clone(&abort);
        let producer_err = Arc::clone(&producer_err);
        let bar = bar.clone();
        // Borrow `files`/`options` from the function scope (outlives the rayon
        // scope); only these references — not the owned Vecs — enter the `move`
        // closure, so the borrowed data stays alive past the scope.
        let files_ref: &[String] = &files;
        let options_ref = &options;
        rayon::scope(move |s| {
            let mut store = store;
            let mut spill = spill;
            let mut pending_nodes = pending_nodes;
            let mut files_indexed = files_indexed;
            let mut files_skipped = files_skipped;
            let mut files_errored = files_errored;
            let mut consumer_err: Option<anyhow::Error> = None;

            // The sole `tx` moves into the producer closure so it drops when
            // parsing ends → the consumer's `rx.recv()` disconnects and exits.
            let producer_gate = Arc::clone(&gate);
            let producer_abort = Arc::clone(&abort);
            let producer_err = Arc::clone(&producer_err);
            s.spawn(move |_| {
                let tx = tx;
                let result = files_ref
                    .par_iter()
                    .enumerate()
                    .progress_with(bar)
                    .try_for_each(|(i, relative)| -> Result<()> {
                        {
                            let (lock, cvar) = &*producer_gate;
                            let mut next_expected = lock.lock().unwrap();
                            while should_block(i, *next_expected, PARSE_REORDER_WINDOW)
                                && !producer_abort.load(Ordering::Relaxed)
                            {
                                next_expected = cvar.wait(next_expected).unwrap();
                            }
                        }
                        if producer_abort.load(Ordering::Relaxed) {
                            return Err(anyhow!("indexing aborted by consumer write error"));
                        }

                        // One metadata + one source read per file (no double read, no
                        // TOCTOU straddle); the size gate mirrors `extract_file`
                        // (engine.rs:152) exactly so oversized files still size-skip.
                        let full = project.join(relative);
                        let metadata = fs::metadata(&full)
                            .with_context(|| format!("reading metadata for {}", full.display()))?;
                        let source = fs::read_to_string(&full)
                            .with_context(|| format!("reading source file {}", full.display()))?;
                        let result = if metadata.len() > options_ref.max_file_size {
                            ExtractionResult {
                                nodes: Vec::new(),
                                edges: Vec::new(),
                                unresolved_references: Vec::new(),
                                errors: vec![format!(
                                    "File exceeds max size ({} > {}): {relative}",
                                    metadata.len(),
                                    options_ref.max_file_size
                                )],
                                duration_ms: 0,
                            }
                        } else {
                            extract_source(relative, &source, None)
                        };
                        let file = FileRecord {
                            path: relative.clone(),
                            content_hash: hash_content(&source),
                            language: detect_language(relative),
                            size: metadata.len() as i64,
                            modified_at: modified_millis(&metadata),
                            indexed_at: now_millis(),
                            node_count: result
                                .nodes
                                .iter()
                                .filter(|n| n.file_path == *relative)
                                .count() as i64,
                            errors: result.errors.clone(),
                        };
                        tx.send((i, relative.clone(), file, result))
                            .map_err(|_| anyhow!("parse result channel disconnected"))?;
                        Ok(())
                    });
                if let Err(err) = result {
                    *producer_err.lock().unwrap() = Some(err);
                }
            });

            // Drain strictly in cursor order via an index-keyed reorder buffer,
            // reproducing the exact sorted-scan persist order. A store-write Err sets
            // `abort`, wakes gated producers, and stops. When `tx` drops, `rx.recv()`
            // disconnects and the loop exits — a missing index (its producer errored)
            // never arrives, so we drain the buffered in-order prefix and stop.
            let mut buffer: ReorderBuffer<ParsePayload> = ReorderBuffer::new();
            let mut next_expected = 0usize;
            'consume: while let Ok(payload) = rx.recv() {
                buffer.insert(payload.0, payload);
                while let Some((_i, _relative, file, mut result)) = buffer.take(next_expected) {
                    if file.errors.is_empty() {
                        files_indexed += 1;
                    } else if result.nodes.is_empty() {
                        files_skipped += 1;
                    } else {
                        files_errored += 1;
                    }

                    let drain = (|| -> Result<()> {
                        store.upsert_file(&file)?;
                        pending_nodes.append(&mut result.nodes);
                        if pending_nodes.len() >= NODE_FLUSH_ROWS {
                            store.upsert_nodes(&pending_nodes)?;
                            pending_nodes.clear();
                        }
                        spill.write_edges(&result.edges)?;
                        spill.write_refs(&result.unresolved_references)?;
                        Ok(())
                    })();
                    if let Err(err) = drain {
                        abort.store(true, Ordering::Relaxed);
                        let (lock, cvar) = &*gate;
                        let _guard = lock.lock().unwrap();
                        cvar.notify_all();
                        consumer_err = Some(err);
                        break 'consume;
                    }

                    next_expected += 1;
                    let (lock, cvar) = &*gate;
                    {
                        let mut ne = lock.lock().unwrap();
                        *ne = next_expected;
                    }
                    cvar.notify_all();
                }
            }

            (
                consumer_err,
                store,
                spill,
                pending_nodes,
                files_indexed,
                files_skipped,
                files_errored,
            )
        })
    };

    // Net behavior MUST equal today's `collect::<Result<Vec>>()?` short-circuit:
    // a consumer write error or any producer parse error returns Err, no hang.
    if let Some(err) = consumer_err {
        return Err(err);
    }
    if let Some(err) = Arc::into_inner(producer_err)
        .expect("producer scope joined; no other Arc holders remain")
        .into_inner()
        .unwrap()
    {
        return Err(err);
    }
    let scan_files = bar_for_finish.position();
    finish_phase(
        &bar_for_finish,
        &format!("Indexed {} files", format_number(scan_files as i64)),
    );

    let pb = phase_spinner("Persisting nodes", quiet);
    if !pending_nodes.is_empty() {
        store.upsert_nodes(&pending_nodes)?;
    }
    drop(pending_nodes);
    finish_phase(&pb, "Persisted nodes");

    let mut spill = spill.into_reader()?;
    let pb = phase_spinner("Persisting edges", quiet);
    spill.replay_edges(EDGE_FLUSH_ROWS, |batch| {
        store.insert_edges(batch).map_err(anyhow::Error::from)
    })?;
    finish_phase(&pb, "Persisted edges");
    let pb = phase_spinner("Persisting references", quiet);
    spill.replay_refs(REF_FLUSH_ROWS, |batch| {
        store
            .insert_unresolved_refs(batch)
            .map_err(anyhow::Error::from)
    })?;
    finish_phase(&pb, "Persisted references");
    spill.cleanup();

    let pb = phase_spinner("Detecting frameworks", quiet);
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
    finish_phase(&pb, "Detected frameworks");
    // Finished from INSIDE the callback on the final chunk so the retained line
    // lands before the resolver's deferred passes (which resolve refs this bar
    // does not count). The trailing finish covers the no-chunk case where the
    // callback never fires; `done_in_callback` prevents a double finish.
    let resolve_bar = progress_bar(
        0,
        quiet,
        "{spinner:.green} Resolving references [{bar:30}] {pos}/{len} ({elapsed})",
    );
    let mut bar_sized = false;
    let mut done_in_callback = false;
    resolver.resolve_and_persist_batched_with_progress(
        &mut store,
        RESOLVE_BATCH_ROWS,
        |processed, total| {
            if !bar_sized {
                resolve_bar.set_length(total);
                bar_sized = true;
            }
            resolve_bar.set_position(processed);
            if processed >= total && !done_in_callback {
                finish_phase(&resolve_bar, "Resolved references");
                done_in_callback = true;
            }
        },
    )?;
    if !done_in_callback {
        finish_phase(&resolve_bar, "Resolved references");
    }
    let pb = phase_spinner("Finalizing frameworks", quiet);
    // Cross-file framework finalization (NestJS RouterModule prefixing) after
    // resolution, mirroring the upstream index.ts:358 runPostExtract.
    resolver.run_post_extract(&mut store)?;
    finish_phase(&pb, "Finalized frameworks");
    store.set_project_metadata("indexed_with_version", VERSION)?;
    store.set_project_metadata(
        "indexed_with_extraction_version",
        &EXTRACTION_VERSION.to_string(),
    )?;
    let pb = phase_spinner("Compacting database", quiet);
    store.compact()?;
    finish_phase(&pb, "Compacted database");
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

/// Index-keyed reorder buffer for the streaming index consumer: parsed payloads
/// arrive out of order and are drained strictly by ascending index, reproducing
/// the serial sorted-scan persist order regardless of parse-completion timing.
struct ReorderBuffer<T> {
    pending: BTreeMap<usize, T>,
}

impl<T> ReorderBuffer<T> {
    fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    fn insert(&mut self, index: usize, payload: T) {
        self.pending.insert(index, payload);
    }

    fn take(&mut self, index: usize) -> Option<T> {
        self.pending.remove(&index)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.pending.len()
    }
}

/// Window-gate predicate: a producer at `index` must wait while it would run
/// more than `window` indices ahead of the consumer's `next_expected` cursor.
/// The head index (`index == next_expected`) is never blocked for `window >= 1`,
/// which is what makes the design deadlock-free.
fn should_block(index: usize, next_expected: usize, window: usize) -> bool {
    index >= next_expected + window
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

/// Collected Godot honesty signals for the matched symbols of one query: the
/// runtime-reachability reasons (so "no static callers" is never reported as
/// dead) and the symbols' own `godot:dynamic:` computed call-sites. Empty for
/// non-Godot projects, which keeps the caller/impact output byte-unchanged.
#[derive(Debug, Default)]
struct GodotHonestySummary {
    reached_via_scene: bool,
    reached_via_autoload: bool,
    dynamic_unresolved: Vec<String>,
}

impl GodotHonestySummary {
    fn has_signal(&self) -> bool {
        self.reached_via_scene || self.reached_via_autoload || !self.dynamic_unresolved.is_empty()
    }

    fn is_dynamically_reachable(&self) -> bool {
        self.reached_via_scene || self.reached_via_autoload
    }

    fn reachability_sources(&self) -> String {
        let mut parts = Vec::new();
        if self.reached_via_scene {
            parts.push("signal/get_node/group");
        }
        if self.reached_via_autoload {
            parts.push("autoload");
        }
        parts.join("/")
    }

    fn as_json(&self) -> serde_json::Value {
        if !self.has_signal() {
            return serde_json::Value::Null;
        }
        json!({
            "dynamicallyReachable": self.is_dynamically_reachable(),
            "reachedViaScene": self.reached_via_scene,
            "reachedViaAutoload": self.reached_via_autoload,
            "dynamicUnresolved": self.dynamic_unresolved,
        })
    }

    fn print_cli(&self, callers_were_empty: bool) {
        if self.is_dynamically_reachable() && callers_were_empty {
            println!(
                "no static callers - may be reached dynamically (Godot {})",
                self.reachability_sources()
            );
        }
        if !self.dynamic_unresolved.is_empty() {
            println!("\ndynamic / unresolved references (cannot be statically confirmed):");
            for name in &self.dynamic_unresolved {
                println!("  {name}");
            }
        }
    }
}

/// Aggregate the Godot dynamic-reachability signal across the exact/top matches
/// for `symbol`. Returns an all-empty summary for any project without Godot
/// links to those matches — the gate that keeps non-Godot output unchanged.
fn godot_honesty_for_symbol(
    store: &Store,
    project: &Path,
    symbol: &str,
) -> Result<GodotHonestySummary> {
    let matches = symbol_matches(store, project, symbol)?;
    let mut summary = GodotHonestySummary::default();
    if matches.is_empty() {
        return Ok(summary);
    }
    let traverser = GraphTraverser::new(store);
    let mut seen = HashSet::new();
    for node in exact_or_top_matches(&matches, symbol) {
        let reach = traverser.godot_dynamic_reachability(node)?;
        for r in &reach.reached_by {
            match r {
                GodotReach::SceneOrResourceLink => summary.reached_via_scene = true,
                GodotReach::Autoload => summary.reached_via_autoload = true,
            }
        }
        for name in reach.dynamic_unresolved {
            if seen.insert(name.clone()) {
                summary.dynamic_unresolved.push(name);
            }
        }
    }
    summary.dynamic_unresolved.sort();
    Ok(summary)
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
    let nodes: Vec<Node> = results.into_iter().map(|r| r.node).collect();
    // GDScript `ClassName.member` qualified-name fallback: when the normal
    // search found no node whose exact `name` equals the queried `symbol` and
    // `symbol` is shaped `<Recv>.<member>`, try to resolve the dotted form to
    // the same `Function` node the short name resolves to. GDScript
    // `class_name X` globals are NOT pushed on the extractor's node stack, so
    // a class method stores `name == qualified_name == <member>` and no dotted
    // node exists — this mirrors the committed T2 resolver
    // (`godot::resolve_class_member`). Returns the resolved nodes directly so
    // callers/impact/query all resolve the dotted form to the exact target.
    if nodes.iter().all(|n| n.name != symbol) {
        if let Some(resolved) = resolve_gdscript_class_member(store, symbol)? {
            if !resolved.is_empty() {
                return Ok(resolved);
            }
        }
    }
    Ok(nodes)
}

/// Resolve a GDScript `<Recv>.<member>` symbol to the `Function` node(s) named
/// `<member>` in the file(s) that define the GDScript `class_name` global named
/// `<Recv>`. Returns `Ok(None)` when `symbol` is not a single-dotted form, when
/// `<Recv>` names no GDScript `Class` node, or when no matching member function
/// exists (the caller then falls back to the normal search results — no
/// regression). Deterministic: class files are sorted lexicographically and
/// deduped, mirroring the T2 resolver's byte-stable ordering.
fn resolve_gdscript_class_member(store: &Store, symbol: &str) -> Result<Option<Vec<Node>>> {
    let Some((receiver, member)) = symbol.split_once('.') else {
        return Ok(None);
    };
    // Only a single-level `<Recv>.<member>` receiver.member shape; a further
    // '.' means a chained/nested access this fallback does not handle.
    if receiver.is_empty() || member.is_empty() || member.contains('.') {
        return Ok(None);
    }

    // (a) GDScript `Class` nodes named `<Recv>`; collect their files.
    let mut class_files: Vec<String> = store
        .nodes_by_name(receiver)?
        .into_iter()
        .filter(|n| n.kind == NodeKind::Class && n.language == Language::Gdscript)
        .map(|n| n.file_path)
        .collect();
    if class_files.is_empty() {
        return Ok(None);
    }
    class_files.sort();
    class_files.dedup();

    // (b) For each class file (sorted), the `<member>` `Function` node(s) in it.
    let member_nodes = store.nodes_by_name(member)?;
    let mut out: Vec<Node> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in &class_files {
        for node in &member_nodes {
            if node.kind == NodeKind::Function
                && node.language == Language::Gdscript
                && &node.file_path == file
                && seen.insert(node.id.clone())
            {
                out.push(node.clone());
            }
        }
    }

    // (c) Return resolved Function nodes, or `None` to fall through.
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
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
        if max_depth.is_none_or(|max| depth <= max) {
            println!(
                "  {} ({}, {} symbols)",
                file.path, file.language, file.node_count
            );
        }
    }
}

#[cfg(test)]
mod self_update_tests {
    use super::{latest_update_tag, should_skip_update};

    #[test]
    fn formats_bare_semver_as_v_prefixed_tag() {
        assert_eq!(latest_update_tag("0.15.0"), "v0.15.0");
        assert_eq!(latest_update_tag("1.2.3"), "v1.2.3");
    }

    #[test]
    fn idempotent_on_already_v_prefixed_input() {
        assert_eq!(latest_update_tag("v0.15.0"), "v0.15.0");
    }

    #[test]
    fn skips_when_current_equals_latest_and_not_forced() {
        assert!(should_skip_update("0.23.0", "0.23.0", false, false));
    }

    #[test]
    fn force_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.23.0", true, false));
    }

    #[test]
    fn newer_latest_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.24.0", false, false));
    }

    #[test]
    fn explicit_tag_never_skips() {
        assert!(!should_skip_update("0.23.0", "0.23.0", false, true));
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::{should_block, ReorderBuffer};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    fn drain_ready(
        buffer: &mut ReorderBuffer<usize>,
        next_expected: &mut usize,
        out: &mut Vec<usize>,
    ) {
        while let Some(payload) = buffer.take(*next_expected) {
            out.push(payload);
            *next_expected += 1;
        }
    }

    #[test]
    fn shuffled_arrival_drains_in_order() {
        let mut buffer = ReorderBuffer::new();
        let mut next_expected = 0usize;
        let mut out = Vec::new();
        for i in [3usize, 1, 0, 2, 4] {
            buffer.insert(i, i);
            drain_ready(&mut buffer, &mut next_expected, &mut out);
        }
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn head_arriving_last_holds_then_releases_all() {
        let mut buffer = ReorderBuffer::new();
        let mut next_expected = 0usize;
        let mut out = Vec::new();
        for i in [1usize, 2, 3, 4] {
            buffer.insert(i, i);
            drain_ready(&mut buffer, &mut next_expected, &mut out);
        }
        assert!(out.is_empty(), "nothing drains until index 0 arrives");
        assert_eq!(buffer.len(), 4);
        buffer.insert(0, 0);
        drain_ready(&mut buffer, &mut next_expected, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn window_gate_blocks_far_index_and_releases_on_advance() {
        let window = 4usize;
        assert!(!should_block(0, 0, window), "head index never blocks");
        assert!(
            !should_block(3, 0, window),
            "last in-window index does not block"
        );
        assert!(should_block(4, 0, window), "index at cursor+window blocks");

        let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
        let abort = Arc::new(AtomicBool::new(false));
        let producer_gate = Arc::clone(&gate);
        let producer_abort = Arc::clone(&abort);
        let index = 4usize;

        let handle = std::thread::spawn(move || {
            let (lock, cvar) = &*producer_gate;
            let mut ne = lock.lock().unwrap();
            while should_block(index, *ne, window) && !producer_abort.load(Ordering::Relaxed) {
                ne = cvar.wait(ne).unwrap();
            }
            *ne
        });

        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !handle.is_finished(),
            "producer at cursor+window stays blocked"
        );

        let (lock, cvar) = &*gate;
        {
            let mut ne = lock.lock().unwrap();
            *ne = 1;
        }
        cvar.notify_all();

        let observed = handle.join().unwrap();
        assert!(
            observed >= 1,
            "producer unblocked after the cursor advanced"
        );
    }

    #[test]
    fn producer_disconnect_with_gap_terminates_consumer() {
        let (tx, rx) = mpsc::channel::<(usize, usize)>();
        tx.send((0, 0)).unwrap();
        tx.send((1, 1)).unwrap();
        tx.send((3, 3)).unwrap();
        drop(tx);

        let mut buffer = ReorderBuffer::new();
        let mut next_expected = 0usize;
        let mut out = Vec::new();
        while let Ok((i, payload)) = rx.recv() {
            buffer.insert(i, payload);
            while let Some(p) = buffer.take(next_expected) {
                out.push(p);
                next_expected += 1;
            }
        }
        assert_eq!(out, vec![0, 1], "drains buffered prefix, stops at the gap");
        assert_eq!(next_expected, 2);
        assert_eq!(buffer.len(), 1, "index 3 stays buffered, never drained");
    }
}
